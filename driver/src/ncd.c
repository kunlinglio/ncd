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


#define DEVICE_NAME "ncd"          // device node name
#define CLASS_NAME  "ncd_class"    // device class name
#define NETLINK_USER 31            // Netlink protocol number

/* Netlink message types */
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
 */
static int send_to_daemon(const char *msg, int len, int type)
{
    struct sk_buff *skb;
    struct nlmsghdr *nlh;
    struct ncd_device *dev = ncd_dev;
    int ret;

    /* Invalid argument */
    if (!dev || dev->user_pid == 0) {
        pr_err("ncd: daemon not ready, PID=%d\n", dev ? dev->user_pid : -1);
        return -EINVAL;
    }

    skb = nlmsg_new(len, GFP_KERNEL);
    /* Out of memory */
    if (!skb) return -ENOMEM;

    nlh = nlmsg_put(skb, 0, 0, type, len, 0);
    /* Out of memory */
    if (!nlh) {
        nlmsg_free(skb);
        return -ENOMEM;
    }
    if (len > 0 && msg) memcpy(nlmsg_data(nlh), msg, len);  // copy payload to message segment

    ret = nlmsg_unicast(nl_sk, skb, dev->user_pid); // send message to daemon
    if (ret < 0)
        pr_err("ncd: nlmsg_unicast error %d\n", ret);
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

    /* record daemon PID if not registered */
    if(dev->user_pid == 0) {
        dev->user_pid = nlh->nlmsg_pid;
        pr_info("ncd: daemon PID=%d registered\n", dev->user_pid);
    }

    switch(type) {
        case NCD_MSG_CONN_RES:
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
            /* receive data from daemon */
            if(nlh->nlmsg_len > NLMSG_HDRLEN) {
                data = (char *)nlmsg_data(nlh);
                len = nlh->nlmsg_len - NLMSG_HDRLEN;

                unsigned int copied = kfifo_in(&dev->data_fifo, data, len);
                if (copied > 0) {
                    /* wake up read wait queue */
                    wake_up_interruptible(&dev->read_wait_queue);
                }
                if (copied < len) {
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
 */
static int ncd_open(struct inode *inode, struct file *filp)
{
    struct ncd_device *dev = container_of(inode->i_cdev, struct ncd_device, cdev);
    int ret;

    /* Ensure exclusive access */
    if (atomic_cmpxchg(&dev->open_count, 0, 1) != 0) {
        pr_err("ncd: device is already opened\n");
        return -EBUSY;
    }

    filp->private_data = dev;

    dev->conn_status = CONN_WAITING;

    ret = send_to_daemon(NULL, 0, NCD_MSG_OPEN_REQ);
    if(ret < 0) {
        pr_err("ncd: send OPEN_REQ failed, err=%d\n", ret);
        return ret;
    }

    while(1) {
        if(dev->conn_status != CONN_WAITING) break;

        ret = wait_event_interruptible(dev->conn_wait_queue, dev->conn_status != CONN_WAITING);
        if(ret) {
            pr_info("ncd: open interrupted by signal\n");
            dev->conn_status = CONN_WAITING;
            atomic_set(&dev->open_count, 0);
            return -ERESTARTSYS;
        }
    }

    if (dev->conn_status == CONN_SUCCESS) {
        ret = 0;
        pr_info("ncd: open succeeded\n");
    } else {
        ret = -ECONNREFUSED;
        pr_err("ncd: open failed, connection refused\n");
        atomic_set(&dev->open_count, 0);
    }
    return ret;
}

static int __init ncd_init(void)
{
    printk(KERN_EMERG "NCD driver init\n");
    return 0;
}

static void __exit ncd_exit(void)
{
    printk(KERN_EMERG "NCD driver exit\n");
}

module_init(ncd_init);
module_exit(ncd_exit);