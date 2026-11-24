CHANNELS ?= 300
RATE     ?= 96000
DURATION ?= 60
SIM_DIRS ?= /tmp/masscap-a /tmp/masscap-b
BIND     ?= 0.0.0.0:8080

.PHONY: build release test sim sim-cli clean

# debug build of whole workspace
build:
	cargo build --workspace

# optimized build
release:
	cargo build --workspace --release

test:
	cargo test --workspace

# web console with simulated input -> http://localhost:8080/
sim:
	cargo run --release -p masscap -- --sim --channels $(CHANNELS) --rate $(RATE) --bind $(BIND)

# headless simulated record into SIM_DIRS
sim-cli:
	mkdir -p $(SIM_DIRS)
	cargo run --release -p masscap-cli -- record --sim --rate $(RATE) --channels $(CHANNELS) \
	  --duration $(DURATION) $(foreach d,$(SIM_DIRS),--dest $(d)) --session Test --cache-mb 4000

clean:
	cargo clean
