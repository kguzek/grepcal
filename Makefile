.PHONY: container-build

container-build:
	GREPCAL_REVISION="$(shell git describe --always --dirty)" podman compose build
