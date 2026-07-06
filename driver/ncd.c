#include <linux/module.h>
#include <linux/init.h>
#include <linux/kernel.h>

MODULE_LICENSE("GPL");
MODULE_DESCRIPTION("NCD driver");
MODULE_AUTHOR("zhegebi");

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