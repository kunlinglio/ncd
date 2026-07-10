#include <linux/module.h>
#include <linux/fs.h>
#include <linux/uaccess.h>
#include <linux/slab.h>
#include <linux/cdev.h>
#include <linux/device.h>
#include <linux/wait.h>
#include <linux/atomic.h>
#include <linux/sched.h>
#include <linux/netlink.h>
#include <linux/skbuff.h>
#include <net/sock.h>
#include <linux/init.h>
#include <linux/kfifo.h>
#include <linux/spinlock.h>

#define DEVICE_NAME "ncd"          // device major name, not a specific device node name
#define CLASS_NAME  "ncd_class"    // device class name
#define NCD_MAX_DEVICES 16         // maximum number of devices
#define NCD_MAX_NAME_LEN 32        // maximum length of device name

#define NETLINK_USER 31            // Netlink protocol number

/* Netlink message types */
#define NCD_MSG_REGISTER        0   // daemon→driver:  get daemon PID
#define NCD_MSG_OPEN_REQ        1   // driver→daemon:  request to open device (and wait for connection)
#define NCD_MSG_CONN_RES        2   // daemon→driver:  connection result (success/fail)
#define NCD_MSG_DATA            3   // bi-directional: data transfer
#define NCD_MSG_CLOSE_REQ       4   // driver→daemon:  request to close device
#define NCD_MSG_CREATE_DEV      5   // daemon→driver:  create device
#define NCD_MSG_DESTROY_DEV     6   // daemon→driver:  destroy device
#define NCD_MSG_KFIFO_FULL      7   // driver→daemon:  kfifo full (80% of buffer size)
#define NCD_MSG_KFIFO_AVAILABLE 8   // driver→daemon:  kfifo available (20% of buffer size)

/* Connection status */
#define CONN_WAITING  0
#define CONN_SUCCESS  1
#define CONN_FAILED  -1

/* Kfifo buffer size */
#define FIFO_SIZE 4096


MODULE_LICENSE("GPL");
MODULE_DESCRIPTION("Network Character Device Driver");
MODULE_AUTHOR("zhegebi");


struct ncd_device {
    struct cdev cdev;
    dev_t dev_num;
    struct device *device;

    wait_queue_head_t conn_wait_queue;      // open wait queue for connection result
    wait_queue_head_t read_wait_queue;      // read wait queue for data

    struct kfifo data_fifo;                 // data buffer for received data from daemon
    spinlock_t fifo_lock;                   // protects data_fifo (kfifo_reset vs kfifo_in)

    int conn_status;                        // Connection status: 0 waiting, 1 success, -1 failed

    atomic_t open_count;                    // Number of processes currently opening the device

    unsigned char minor;                    // minor device number, which means the array index
    bool in_use;                            // whether the array index is in use
    bool kfifo_paused;                      // daemon has been told to pause (kfifo ≥ 80%)
    char name[NCD_MAX_NAME_LEN];            // device node name
};

static struct ncd_device *ncd_devices[NCD_MAX_DEVICES];
static struct class *ncd_class;
static pid_t daemon_pid;
static dev_t ncd_major_dev; // major device number(minor = 0)
static struct sock *nl_sk = NULL;
static struct file_operations ncd_file_operations;



/**
 * send_to_daemon - send Netlink message to daemon
 * @msg: data payload (can be NULL)
 * @len: payload length
 * @type: message type (NCD_MSG_*)
 * return 0 on success, negative error code on failure
 */
static int send_to_daemon(const char *msg, int len, int type)
{
    struct sk_buff *skb;
    struct nlmsghdr *nlh;
    int ret;

    /* Invalid argument */
    if(daemon_pid == 0) {
        pr_err("ncd: daemon not ready\n");
        return -EINVAL;
    }

    skb = nlmsg_new(len, GFP_ATOMIC);
    /* Out of memory */
    if(!skb) return -ENOMEM;

    nlh = nlmsg_put(skb, 0, 0, type, len, 0);
    /* Out of memory */
    if(!nlh) {
        nlmsg_free(skb);
        return -ENOMEM;
    }
    if(len > 0 && msg) memcpy(nlmsg_data(nlh), msg, len);  // copy payload to message segment

    ret = nlmsg_unicast(nl_sk, skb, daemon_pid); // send message to daemon
    if(ret < 0) {
        pr_err("ncd: nlmsg_unicast error: %d, the daemon may be dead\n", ret);
        daemon_pid = 0;  // daemon is dead, allow re-registration
    }
    return ret;
}


/**
 * recv_from_daemon - receive Netlink message 
 * cannot sleep because it is running in soft interrupt context
 */
static void recv_from_daemon(struct sk_buff *skb)
{
    struct nlmsghdr *nlh = (struct nlmsghdr *)skb->data;
    int type = nlh->nlmsg_type;
    char* data;
    int len;
    int ret;

    switch(type) {
        case NCD_MSG_REGISTER:
            if(daemon_pid != 0) {
                pr_info("ncd: daemon restart detected, cleaning old devices\n");
                /* clean up all old devices before re-registration */
                for (int i = 0; i < NCD_MAX_DEVICES; i++) {
                    if (ncd_devices[i] && ncd_devices[i]->in_use) {
                        device_destroy(ncd_class, ncd_devices[i]->dev_num);
                        kfifo_free(&ncd_devices[i]->data_fifo);
                        cdev_del(&ncd_devices[i]->cdev);
                        ncd_devices[i]->in_use = false;
                    }
                }
            }
            /* record daemon PID*/
            daemon_pid = nlh->nlmsg_pid;
            pr_info("ncd: daemon PID=%d registered\n", daemon_pid);
            break;
        case NCD_MSG_CONN_RES:
            if(nlh->nlmsg_pid != daemon_pid) {
                pr_warn("ncd: unknown PID %d, ignoring\n", nlh->nlmsg_pid);
                break;
            }
            /* receive connection result from daemon */
            if(nlh->nlmsg_len > NLMSG_HDRLEN + 1) {
                data = (char *)nlmsg_data(nlh);
                len = nlh->nlmsg_len - NLMSG_HDRLEN;

                unsigned char minor = data[0];
                if (minor >= NCD_MAX_DEVICES || !ncd_devices[minor] || !ncd_devices[minor]->in_use) break;

                if(data[1] == '1') {
                    ncd_devices[minor]->conn_status = CONN_SUCCESS;
                    pr_info("ncd: client connected\n");
                } else {
                    ncd_devices[minor]->conn_status = CONN_FAILED;
                    pr_err("ncd: client connection failed\n");
                }

                /* wake up open wait queue */
                wake_up_interruptible(&ncd_devices[minor]->conn_wait_queue);
            }
            break;
        case NCD_MSG_DATA:
            if(nlh->nlmsg_pid != daemon_pid) {
                pr_warn("ncd: unknown PID %d, ignoring\n", nlh->nlmsg_pid);
                break;
            }
            /* receive data from daemon */
            if(nlh->nlmsg_len > NLMSG_HDRLEN) {
                data = (char *)nlmsg_data(nlh);
                len = nlh->nlmsg_len - NLMSG_HDRLEN;

                unsigned char minor = data[0];
                if (minor >= NCD_MAX_DEVICES || !ncd_devices[minor] || !ncd_devices[minor]->in_use) {
                    pr_warn("ncd: unknown minor %d, ignoring\n", minor);
                    break;
                }

                spin_lock(&ncd_devices[minor]->fifo_lock);
                unsigned int copied = kfifo_in(&ncd_devices[minor]->data_fifo, data + 1, len - 1);
                unsigned int used = kfifo_len(&ncd_devices[minor]->data_fifo);
                spin_unlock(&ncd_devices[minor]->fifo_lock);

                if(copied > 0) {
                    /* wake up read wait queue */
                    wake_up_interruptible(&ncd_devices[minor]->read_wait_queue);

                    /* backpressure: pause daemon when kfifo ≥ 80% full */
                    if (used >= FIFO_SIZE * 80 / 100 && !ncd_devices[minor]->kfifo_paused) {
                        ncd_devices[minor]->kfifo_paused = true;
                        char pause[1] = { minor };
                        send_to_daemon(pause, 1, NCD_MSG_KFIFO_FULL);
                        pr_info("ncd: kfifo %d%% full, pausing daemon for minor %d\n",
                                used * 100 / FIFO_SIZE, minor);
                    }
                }
                if(copied < len - 1) {
                    pr_warn("ncd: kfifo full, dropped %u bytes\n", len - 1 - copied);
                }
            }
            break;
        case NCD_MSG_CREATE_DEV:
            if(nlh->nlmsg_pid != daemon_pid) {
                pr_warn("ncd: unknown PID %d, ignoring\n", nlh->nlmsg_pid);
                break;
            }
            /* payload = minor(1 byte) + device name */
            if(nlh->nlmsg_len <= NLMSG_HDRLEN + 1) {
                pr_err("ncd: invalid CREATE_DEV payload, ignoring\n");
                break;
            }
            data = (char *)nlmsg_data(nlh);
            len = nlh->nlmsg_len - NLMSG_HDRLEN;
            unsigned char minor = data[0];
            if(minor >= NCD_MAX_DEVICES) {
                pr_err("ncd: invalid minor %d, ignoring\n", minor);
                break;
            }
            if(ncd_devices[minor] && ncd_devices[minor]->in_use) {
                pr_err("ncd: minor %d already in use, ignoring\n", minor);
                break;
            }
            {
                char *name = data + 1;
                int name_len = len - 1;
                name_len = name_len >= NCD_MAX_NAME_LEN ? NCD_MAX_NAME_LEN - 1 : name_len;

                if(!ncd_devices[minor]) {
                    ncd_devices[minor] = kzalloc(sizeof(struct ncd_device), GFP_ATOMIC);
                    if(!ncd_devices[minor]) {
                        pr_err("ncd: kzalloc failed, error: %d \n", -ENOMEM);
                        break;
                    }
                }

                /* initialize device properties */
                ncd_devices[minor]->minor = minor;
                ncd_devices[minor]->in_use = true;
                memcpy(ncd_devices[minor]->name, name, name_len);
                ncd_devices[minor]->name[name_len] = '\0';
                ncd_devices[minor]->dev_num = MKDEV(MAJOR(ncd_major_dev), minor);

                /* initialize cdev */
                cdev_init(&ncd_devices[minor]->cdev, &ncd_file_operations);
                ncd_devices[minor]->cdev.owner = THIS_MODULE;
                ret = cdev_add(&ncd_devices[minor]->cdev, ncd_devices[minor]->dev_num, 1);
                if(ret < 0) {
                    pr_err("ncd: cdev_add failed, error: %d\n", ret);
                    ncd_devices[minor]->in_use = false;
                    break;
                }

                /* initialize synchronization primitives */
                init_waitqueue_head(&ncd_devices[minor]->conn_wait_queue);
                init_waitqueue_head(&ncd_devices[minor]->read_wait_queue);
                spin_lock_init(&ncd_devices[minor]->fifo_lock);
                atomic_set(&ncd_devices[minor]->open_count, 0);
                ncd_devices[minor]->conn_status = CONN_WAITING;
                ncd_devices[minor]->kfifo_paused = false;

                /* kfifo alloc */
                ret = kfifo_alloc(&ncd_devices[minor]->data_fifo, FIFO_SIZE, GFP_ATOMIC);
                if(ret < 0) {
                    pr_err("ncd: kfifo_alloc failed, ret=%d\n", ret);
                    cdev_del(&ncd_devices[minor]->cdev);
                    ncd_devices[minor]->in_use = false;
                    break;
                }

                /* create device */
                ncd_devices[minor]->device = device_create(ncd_class, NULL,
                                                          ncd_devices[minor]->dev_num, NULL,
                                                          ncd_devices[minor]->name);
                if(IS_ERR(ncd_devices[minor]->device)) {
                    ret = PTR_ERR(ncd_devices[minor]->device);
                    pr_err("ncd: device_create failed, error: %d\n", ret);
                    kfifo_free(&ncd_devices[minor]->data_fifo);
                    cdev_del(&ncd_devices[minor]->cdev);
                    ncd_devices[minor]->in_use = false;
                    break;
                }
                pr_info("ncd: device /dev/%s created (minor=%d)\n",
                        ncd_devices[minor]->name, minor);
            }
            break;
        case NCD_MSG_DESTROY_DEV:
            if(nlh->nlmsg_pid != daemon_pid) {
                pr_warn("ncd: unknown PID %d, ignoring\n", nlh->nlmsg_pid);
                break;
            }
            /* receive minor from daemon and destroy device */
            if(nlh->nlmsg_len > NLMSG_HDRLEN) {
                data = (char *)nlmsg_data(nlh);
                unsigned char minor = data[0];
                if (minor >= NCD_MAX_DEVICES || !ncd_devices[minor] || !ncd_devices[minor]->in_use) {
                    pr_warn("ncd: unknown minor %d, ignoring\n", minor);
                    break;
                }
                device_destroy(ncd_class, ncd_devices[minor]->dev_num);
                kfifo_free(&ncd_devices[minor]->data_fifo);
                cdev_del(&ncd_devices[minor]->cdev);
                ncd_devices[minor]->in_use = false;
            }
            break;
        default:
            pr_warn("ncd: unknown netlink msg type %d\n", type);
            break;
    }
}


/**
 * ncd_open - wait for connection result from daemon
 * return 0 on success, negative error code on failure
 */
static int ncd_open(struct inode *inode, struct file *filp)
{
    struct ncd_device *dev = container_of(inode->i_cdev, struct ncd_device, cdev);
    int ret;

    /* Ensure exclusive access */
    if(atomic_cmpxchg(&dev->open_count, 0, 1) != 0) {
        pr_err("ncd: device is already opened\n");
        return -EBUSY;
    }

    filp->private_data = dev;

    dev->conn_status = CONN_WAITING;

    char payload[1] = {dev->minor};

    ret = send_to_daemon(payload, 1, NCD_MSG_OPEN_REQ);
    if(ret < 0) {
        pr_err("ncd: send OPEN_REQ failed, err=%d\n", ret);
        atomic_set(&dev->open_count, 0);
        return ret;
    }

    ret = wait_event_interruptible(dev->conn_wait_queue, dev->conn_status != CONN_WAITING);
    if(ret) {
        pr_info("ncd: open interrupted by signal\n");
        dev->conn_status = CONN_WAITING;
        atomic_set(&dev->open_count, 0);
        return -ERESTARTSYS;
    }

    if(dev->conn_status == CONN_SUCCESS) {
        ret = 0;
        pr_info("ncd: open succeeded\n");
    } else {
        ret = -ECONNREFUSED;
        pr_err("ncd: open failed, connection refused\n");
        atomic_set(&dev->open_count, 0);
    }
    return ret;
}


/**
 * ncd_read - read data from kfifo
 * return number of bytes read, negative error code on failure
 */
static ssize_t ncd_read(struct file *filp, char __user *buf, size_t count, loff_t *f_pos)
{
    struct ncd_device *dev = filp->private_data;
    unsigned int copied;
    unsigned long flags;
    int ret;

    if(!dev) {
        pr_err("ncd: read with NULL private_data\n");
        return -EINVAL;
    }

    /* Wait until fifo has data; re-check under lock to prevent
     * race with kfifo_reset() in ncd_release(). */
    while(1) {
        spin_lock_irqsave(&dev->fifo_lock, flags);
        if(!kfifo_is_empty(&dev->data_fifo)) break;
        spin_unlock_irqrestore(&dev->fifo_lock, flags);

        ret = wait_event_interruptible(dev->read_wait_queue, !kfifo_is_empty(&dev->data_fifo));
        if(ret) {
            pr_info("ncd: read interrupted by signal\n");
            return -ERESTARTSYS;
        }
    }

    ret = kfifo_to_user(&dev->data_fifo, buf, count, &copied);
    spin_unlock_irqrestore(&dev->fifo_lock, flags);

    if(ret) {
        pr_err("ncd: kfifo_to_user failed, ret=%d\n", ret);
        return -EFAULT;
    }

    /* resume daemon when kfifo drops below 20% */
    if (dev->kfifo_paused) {
        unsigned int used = kfifo_len(&dev->data_fifo);
        if (used < FIFO_SIZE * 20 / 100) {
            dev->kfifo_paused = false;
            char resume[1] = { dev->minor };
            send_to_daemon(resume, 1, NCD_MSG_KFIFO_AVAILABLE);
            pr_info("ncd: kfifo %d%% free, resuming daemon for minor %d\n",
                    used * 100 / FIFO_SIZE, dev->minor);
        }
    }

    return copied;
}


/**
 * ncd_write - send data to daemon
 * return number of bytes written, negative error code on failure
 */
static ssize_t ncd_write(struct file *filp, const char __user *buf, size_t count, loff_t *f_pos)
{
    struct ncd_device *dev = filp->private_data;
    char* kbuf;
    int ret;

    if(count == 0) return 0;

    kbuf = kmalloc(count + 1, GFP_KERNEL);
    /* Out of memory */
    if(!kbuf) return -ENOMEM;

    kbuf[0] = dev->minor;

    if(copy_from_user(kbuf + 1, buf, count)) {
        kfree(kbuf);
        return -EFAULT;
    }

    ret = send_to_daemon(kbuf, count + 1, NCD_MSG_DATA);
    kfree(kbuf);

    return ret < 0 ? ret : count;
}


/**
 * ncd_release - release device, free exclusive access
 * return 0 on success, negative error code on failure
 */
static int ncd_release(struct inode *inode, struct file *filp)
{
    struct ncd_device *dev = filp->private_data;
    unsigned long flags;

    if(!dev) return 0;

    char payload[1] = {dev->minor};

    send_to_daemon(payload, 1, NCD_MSG_CLOSE_REQ);

    dev->conn_status = CONN_WAITING;

    spin_lock_irqsave(&dev->fifo_lock, flags);
    kfifo_reset(&dev->data_fifo);
    spin_unlock_irqrestore(&dev->fifo_lock, flags);

    atomic_set(&dev->open_count, 0);
    dev->kfifo_paused = false;

    pr_info("ncd: device released\n");
    return 0;
}


static struct file_operations ncd_file_operations = {
    .owner = THIS_MODULE,
    .open = ncd_open,
    .read = ncd_read,
    .write = ncd_write,
    .release = ncd_release
};

static int __init ncd_init(void)
{
    int ret;

    /* 1. Allocate major device number (reserve NCD_MAX_DEVICES minors) */
    ret = alloc_chrdev_region(&ncd_major_dev, 0, NCD_MAX_DEVICES, DEVICE_NAME);
    if(ret < 0) {
        pr_err("ncd: alloc_chrdev_region failed, error: %d\n", ret);
        return ret;
    }

    /* 2. Create device class */
    ncd_class = class_create(CLASS_NAME);
    if(IS_ERR(ncd_class)) {
        ret = PTR_ERR(ncd_class);
        pr_err("ncd: class_create failed, error: %d\n", ret);
        unregister_chrdev_region(ncd_major_dev, NCD_MAX_DEVICES);
        return ret;
    }

    /* 3. Create netlink socket (devices are created later via daemon) */
    struct netlink_kernel_cfg cfg = {
        .input = recv_from_daemon,
    };
    nl_sk = netlink_kernel_create(&init_net, NETLINK_USER, &cfg);
    if(!nl_sk) {
        pr_err("ncd: netlink_kernel_create failed\n");
        class_destroy(ncd_class);
        unregister_chrdev_region(ncd_major_dev, NCD_MAX_DEVICES);
        return -ENOMEM;
    }

    pr_info("ncd: driver initialized (major=%d, max_devices=%d)\n",
            MAJOR(ncd_major_dev), NCD_MAX_DEVICES);
    return 0;
}

static void __exit ncd_exit(void)
{
    /* 1. Release netlink socket (stops callbacks immediately) */
    if(nl_sk) {
        netlink_kernel_release(nl_sk);
        nl_sk = NULL;
    }

    /* 2. Destroy all active devices */
    for(int i = 0; i < NCD_MAX_DEVICES; i++) {
        struct ncd_device *dev = ncd_devices[i];
        if(!dev || !dev->in_use) continue;

        device_destroy(ncd_class, dev->dev_num);
        kfifo_free(&dev->data_fifo);
        cdev_del(&dev->cdev);
        kfree(dev);
        ncd_devices[i] = NULL;
    }

    /* 3. Release class and major device number */
    if(ncd_class) {
        class_destroy(ncd_class);
        ncd_class = NULL;
    }
    unregister_chrdev_region(ncd_major_dev, NCD_MAX_DEVICES);

    pr_info("ncd: driver unloaded\n");
}

module_init(ncd_init);
module_exit(ncd_exit);