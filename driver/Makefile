# Kernel source path
KDIR := /lib/modules/$(shell uname -r)/build

# Current working directory
PWD := $(shell pwd)

# Build directory (all artifacts go here)
BUILD_DIR := $(PWD)/build

# ---- Module configuration ----
obj-m := ncd.o

# Include paths
ccflags-y := -I$(PWD)/includes
# -------------------------------

# Default target: build the module
default:
	@mkdir -p $(BUILD_DIR)
	@cp $(PWD)/src/*.c $(BUILD_DIR)/
	@echo "obj-m := $(obj-m)"            >  $(BUILD_DIR)/Makefile
	@echo 'ccflags-y := $(ccflags-y)'    >> $(BUILD_DIR)/Makefile
	$(MAKE) -C $(KDIR) M=$(BUILD_DIR) modules

# Clean: remove all build artifacts
clean:
	@$(MAKE) -C $(KDIR) M=$(BUILD_DIR) clean 2>/dev/null || true
	@rm -rf $(BUILD_DIR)
