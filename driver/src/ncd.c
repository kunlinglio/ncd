#include <linux/module.h>
#include <linux/fs.h>
#include <linux/uaccess.h>
#include <linux/slab.h>
#include <linux/cdev.h>
#include <linux/device.h>
#include <linux/wait.h>
#include <linux/mutex.h>
#include <linux/sched.h>
#include <linux/netlink.h>
#include <linux/skbuff.h>
#include <net/sock.h>
#include <linux/init.h>


#define DEVICE_NAME "ncd"          // device node name
#define CLASS_NAME  "ncd_class"    // device class name
#define NETLINK_USER 31            // Netlink protocol number

/* Netlink message types */
#define NCD_MSG_OPEN_REQ   1       // driver→daemon: request to open device (and wait for connection)
#define NCD_MSG_CONN_RES   2       // driver→daemon: connection result (success/fail)
#define NCD_MSG_DATA       3       // bi-directional data transfer
#define NCD_MSG_CLOSE_REQ  4       // driver→daemon: request to close device

/* Connection status */
#define CONN_WAITING  0
#define CONN_SUCCESS  1
#define CONN_FAILED  -1


MODULE_LICENSE("GPL");
MODULE_DESCRIPTION("Network Character Device Driver");
MODULE_AUTHOR("zhegebi");


struct ncd_device {
    struct cdev cdev;
    dev_t dev_num;
    struct class *class;
    struct device *device;

    wait_queue_head_t conn_wq;      // open wait queue for connection result
    wait_queue_head_t read_wq;      // read wait queue for data
    struct mutex lock;              // mutex for shared data protection

    char *rx_buffer;                // data buffer for received data from daemon
    size_t rx_len;                  // valid data length

    pid_t user_pid;                 // Daemon PID (Netlink callback records)
    int conn_status;                // Connection status: 0 waiting, 1 success, -1 failed
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
        pr_err("ncd: daemon not ready, PID=%d\n", dev->user_pid);
        return -EINVAL;
    }

    skb = nlmsg_new(len, GFP_KERNEL);
    if (!skb)
        return -ENOMEM;

    nlh = nlmsg_put(skb, 0, 0, type, len, 0);
    if (!nlh) {
        nlmsg_free(skb);
        return -ENOMEM;
    }
    if (len > 0 && msg)
        memcpy(nlmsg_data(nlh), msg, len);

    ret = nlmsg_unicast(nl_sk, skb, dev->user_pid);
    if (ret < 0)
        pr_err("ncd: nlmsg_unicast error %d\n", ret);
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