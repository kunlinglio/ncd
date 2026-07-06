# Compiler selection
CC := gcc

# Target module (renamed to ncd)
obj-m := ncd.o

# Kernel source path
KDIR := /lib/modules/$(shell uname -r)/build

# Current working directory
PWD := $(shell pwd)

# Output directory for final artifacts
OUTPUT := ./build

# Default target: build module, then copy artifacts
default:
	$(MAKE) -C $(KDIR) M=$(PWD) modules
	mkdir -p $(OUTPUT)
	cp ncd.ko $(OUTPUT)/
	@echo "Module copied to $(OUTPUT)/"

# Clean: remove build artifacts and output directory
clean:
	$(MAKE) -C $(KDIR) M=$(PWD) clean
	rm -rf $(OUTPUT)