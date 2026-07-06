# Compiler selection
CC := gcc

# Target module
obj-m := ncd.o

# Kernel source path
KDIR := /lib/modules/$(shell uname -r)/build

# Current working directory
PWD := $(shell pwd)

# Default target: build the module
default:
	$(MAKE) -C $(KDIR) M=$(PWD) modules

# Clean: remove build artifacts
clean:
	$(MAKE) -C $(KDIR) M=$(PWD) clean