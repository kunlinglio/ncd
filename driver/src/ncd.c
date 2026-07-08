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


#define DEVICE_NAME "ncd"          // device node name
#define CLASS_NAME  "ncd_class"    // device class name
#define NETLINK_USER 31            // Netlink protocol number

/* Netlink message types */
#define NCD_MSG_REGISTER   0       // daemon→driver:  get daemon PID
#define NCD_MSG_OPEN_REQ   1       // driver→daemon:  request to open device (and wait for connection)
#define NCD_MSG_CONN_RES   2       // daemon→driver:  connection result (success/fail)
#define NCD_MSG_DATA       3       // bi-directional: data transfer
#define NCD_MSG_CLOSE_REQ  4       // driver→daemon:  request to close device

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
    struct class *class;
    struct device *device;

    wait_queue_head_t conn_wait_queue;      // open wait queue for connection result
    wait_queue_head_t read_wait_queue;      // read wait queue for data

    struct kfifo data_fifo;                 // data buffer for received data from daemon
    spinlock_t fifo_lock;                   // protects data_fifo (kfifo_reset vs kfifo_in)

    pid_t user_pid;                         // Daemon PID (Netlink callback records)
    int conn_status;                        // Connection status: 0 waiting, 1 success, -1 failed

    atomic_t open_count;                    // Number of processes currently opening the device
};

static struct ncd_device *ncd_dev;
static struct sock *nl_sk = NULL;


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
    struct ncd_device *dev = ncd_dev;
    int ret;

    /* Invalid argument */
    if(!dev || dev->user_pid == 0) {
        pr_err("ncd: daemon not ready, PID=%d\n", dev ? dev->user_pid : -1);
        return -EINVAL;
    }

    skb = nlmsg_new(len, GFP_KERNEL);
    /* Out of memory */
    if(!skb) return -ENOMEM;

    nlh = nlmsg_put(skb, 0, 0, type, len, 0);
    /* Out of memory */
    if(!nlh) {
        nlmsg_free(skb);
        return -ENOMEM;
    }
    if(len > 0 && msg) memcpy(nlmsg_data(nlh), msg, len);  // copy payload to message segment

    ret = nlmsg_unicast(nl_sk, skb, dev->user_pid); // send message to daemon
    if(ret < 0) {
        pr_err("ncd: nlmsg_unicast error: %d, the daemon may be dead\n", ret);
        dev->user_pid = 0;  // daemon is dead, allow re-registration
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
    struct ncd_device *dev = ncd_dev;
    int type = nlh->nlmsg_type;
    char* data;
    int len;

    if(!dev) return;

    switch(type) {
        case NCD_MSG_REGISTER:
            if(dev->user_pid != 0) {
                pr_warn("ncd: daemon already registered (PID=%d), ignoring\n", dev->user_pid);
                break;
            }
            /* record daemon PID*/
            dev->user_pid = nlh->nlmsg_pid;
            pr_info("ncd: daemon PID=%d registered\n", dev->user_pid);
            break;
        case NCD_MSG_CONN_RES:
            if(nlh->nlmsg_pid != dev->user_pid) break;
            /* receive connection result from daemon */
            if(nlh->nlmsg_len > NLMSG_HDRLEN) {
                data = (char *)nlmsg_data(nlh);
                len = nlh->nlmsg_len - NLMSG_HDRLEN;

                if(data[0] == '1') {
                    dev->conn_status = CONN_SUCCESS;
                    pr_info("ncd: client connected\n");
                } else {
                    dev->conn_status = CONN_FAILED;
                    pr_err("ncd: client connection failed\n");
                }

                /* wake up open wait queue */
                wake_up_interruptible(&dev->conn_wait_queue);
            }
            break;
        case NCD_MSG_DATA:
            if(nlh->nlmsg_pid != dev->user_pid) break;
            /* receive data from daemon */
            if(nlh->nlmsg_len > NLMSG_HDRLEN) {
                data = (char *)nlmsg_data(nlh);
                len = nlh->nlmsg_len - NLMSG_HDRLEN;

                spin_lock(&dev->fifo_lock);
                unsigned int copied = kfifo_in(&dev->data_fifo, data, len);
                spin_unlock(&dev->fifo_lock);

                if(copied > 0) {
                    /* wake up read wait queue */
                    wake_up_interruptible(&dev->read_wait_queue);
                }
                if(copied < len) {
                    pr_warn("ncd: kfifo full, dropped %u bytes\n", len - copied);
                }
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

    ret = send_to_daemon(NULL, 0, NCD_MSG_OPEN_REQ);
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

    return copied;
}


/**
 * ncd_write - send data to daemon
 * return number of bytes written, negative error code on failure
 */
static ssize_t ncd_write(struct file *filp, const char __user *buf, size_t count, loff_t *f_pos)
{
    char* kbuf;
    int ret;

    if(count == 0) return 0;

    kbuf = kmalloc(count + 1, GFP_KERNEL);
    /* Out of memory */
    if(!kbuf) return -ENOMEM;

    if(copy_from_user(kbuf, buf, count)) {
        kfree(kbuf);
        return -EFAULT;
    }

    kbuf[count] = '\0'; // used for print debug log

    ret = send_to_daemon(kbuf, count, NCD_MSG_DATA);
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

    send_to_daemon(NULL, 0, NCD_MSG_CLOSE_REQ);

    dev->conn_status = CONN_WAITING;

    spin_lock_irqsave(&dev->fifo_lock, flags);
    kfifo_reset(&dev->data_fifo);
    spin_unlock_irqrestore(&dev->fifo_lock, flags);

    atomic_set(&dev->open_count, 0);

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

    /* 1. Allocate device structure */
    ncd_dev = kzalloc(sizeof(struct ncd_device), GFP_KERNEL);
    if(!ncd_dev) {
        ret = -ENOMEM;
        return ret;
    }

    /* 2. Register character device dynamically */
    ret = alloc_chrdev_region(&ncd_dev->dev_num, 0, 1, DEVICE_NAME);
    if(ret < 0) {
        pr_err("ncd: alloc_chrdev_region failed\n");
        kfree(ncd_dev);
        ncd_dev = NULL;
        return ret;
    }

    /* 3. Initialize cdev */
    cdev_init(&ncd_dev->cdev, &ncd_file_operations);
    ncd_dev->cdev.owner = THIS_MODULE;
    ret = cdev_add(&ncd_dev->cdev, ncd_dev->dev_num, 1);
    if(ret < 0) {
        pr_err("ncd: cdev_add failed\n");
        unregister_chrdev_region(ncd_dev->dev_num, 1);
        kfree(ncd_dev);
        ncd_dev = NULL;
        return ret;
    }

    /* 4. Initialize waitqueue, spinlock, atomic variable and kfifo */
    init_waitqueue_head(&ncd_dev->conn_wait_queue);
    init_waitqueue_head(&ncd_dev->read_wait_queue);
    spin_lock_init(&ncd_dev->fifo_lock);
    atomic_set(&ncd_dev->open_count, 0);
    ncd_dev->user_pid = 0;
    ncd_dev->conn_status = CONN_WAITING;
    ret = kfifo_alloc(&ncd_dev->data_fifo, FIFO_SIZE, GFP_KERNEL);
    if(ret < 0) {
        pr_err("ncd: kfifo_alloc failed, ret=%d\n", ret);
        cdev_del(&ncd_dev->cdev);
        unregister_chrdev_region(ncd_dev->dev_num, 1);
        kfree(ncd_dev);
        ncd_dev = NULL;
        return ret;
    }

    /* 5. Create class and device node */
    ncd_dev->class = class_create(CLASS_NAME);
    if(IS_ERR(ncd_dev->class)) {
        ret = PTR_ERR(ncd_dev->class);
        pr_err("ncd: class_create failed\n");
        kfifo_free(&ncd_dev->data_fifo);
        cdev_del(&ncd_dev->cdev);
        unregister_chrdev_region(ncd_dev->dev_num, 1);
        kfree(ncd_dev);
        ncd_dev = NULL;
        return ret;
    }
    ncd_dev->device = device_create(ncd_dev->class, NULL, ncd_dev->dev_num, 
                                NULL, DEVICE_NAME);
    if(IS_ERR(ncd_dev->device)) {
        ret = PTR_ERR(ncd_dev->device);
        pr_err("ncd: device_create failed\n");
        class_destroy(ncd_dev->class);
        kfifo_free(&ncd_dev->data_fifo);
        cdev_del(&ncd_dev->cdev);
        unregister_chrdev_region(ncd_dev->dev_num, 1);
        kfree(ncd_dev);
        ncd_dev = NULL;
        return ret;
    }

    /* 6. Create netlink socket */
    struct netlink_kernel_cfg cfg = {
        .input = recv_from_daemon,
    };
    nl_sk = netlink_kernel_create(&init_net, NETLINK_USER, &cfg);
    if(!nl_sk) {
        pr_err("ncd: netlink_kernel_create failed\n");
        ret = -ENOMEM;
        device_destroy(ncd_dev->class, ncd_dev->dev_num);
        class_destroy(ncd_dev->class);
        kfifo_free(&ncd_dev->data_fifo);
        cdev_del(&ncd_dev->cdev);
        unregister_chrdev_region(ncd_dev->dev_num, 1);
        kfree(ncd_dev);
        ncd_dev = NULL;
        return ret;
    }

    pr_info("ncd: driver initialized (major=%d, fifo=%d)\n", MAJOR(ncd_dev->dev_num), FIFO_SIZE);
    return 0;
}

static void __exit ncd_exit(void)
{
    /* 1. Release netlink socket */
    if(nl_sk) {
        netlink_kernel_release(nl_sk);
        nl_sk = NULL;
    }

    /* 2. Release device resources */
    if(ncd_dev) {
        device_destroy(ncd_dev->class, ncd_dev->dev_num);
        class_destroy(ncd_dev->class);
        kfifo_free(&ncd_dev->data_fifo);
        cdev_del(&ncd_dev->cdev);
        unregister_chrdev_region(ncd_dev->dev_num, 1);
        kfree(ncd_dev);
        ncd_dev = NULL;
    }

    pr_info("ncd: driver unloaded\n");
}

module_init(ncd_init);
module_exit(ncd_exit);