# Firecracker MicroVM Isolation

Run OpenAB agents inside [Firecracker](https://github.com/firecracker-microvm/firecracker) microVMs for kernel-level isolation. Each agent gets its own lightweight VM with an independent kernel — the strongest sandbox boundary available without dedicated hardware.

## Why Firecracker

| Isolation model | Boundary | Kernel exploit risk |
|---|---|---|
| Container (runc) | Linux namespaces + cgroups | Shared kernel — vulnerable |
| Container + Landlock/seccomp (OpenShell) | Above + syscall filtering | Shared kernel — reduced surface |
| **Firecracker microVM** | **Independent guest kernel** | **Host kernel not exposed to agent** |

Firecracker was built by AWS for Lambda and Fargate — designed to run untrusted code with VM-level isolation and container-like startup speed (~125ms).

## Prerequisites

- **x86_64 host with KVM** — Intel VT-x (or AMD-V). The Intel N100 supports VT-x.
- **K3s** installed and running
- **KVM enabled** — verify with:

```bash
ls /dev/kvm
# Should exist. If not:
sudo modprobe kvm_intel   # Intel
sudo modprobe kvm_amd     # AMD
```

## Install Kata Containers on K3s

[Kata Containers](https://katacontainers.io/) provides the integration layer between Kubernetes and Firecracker. The `kata-deploy` DaemonSet installs all binaries (Firecracker, kata-runtime, guest kernel, rootfs) and configures containerd automatically.

```bash
# K3s-specific overlay handles containerd config differences
kubectl apply -k https://github.com/kata-containers/kata-containers/tools/packaging/kata-deploy/kata-deploy/overlays/k3s

# Wait for installation to complete
kubectl -n kube-system wait --timeout=10m --for=condition=Ready -l name=kata-deploy pod

# Install RuntimeClasses (kata-fc, kata-qemu, kata-clh)
kubectl apply -f https://raw.githubusercontent.com/kata-containers/kata-containers/main/tools/packaging/kata-deploy/runtimeclasses/kata-runtimeClasses.yaml
```

## Verify

```bash
# Check node label
kubectl get nodes -l katacontainers.io/kata-runtime=true

# Run a test pod with Firecracker
kubectl apply -f https://raw.githubusercontent.com/kata-containers/kata-containers/main/tools/packaging/kata-deploy/examples/test-deploy-kata-fc.yaml

# Verify it's running
kubectl get pods -l app=kata-fc-test

# Cleanup
kubectl delete -f https://raw.githubusercontent.com/kata-containers/kata-containers/main/tools/packaging/kata-deploy/examples/test-deploy-kata-fc.yaml
```

## Configure OpenAB to Use Firecracker

Add `runtimeClassName: kata-fc` to your Helm values:

```bash
helm install openab openab/openab \
  --set podRuntimeClassName=kata-fc \
  --set agents.kiro.discord.botToken="$DISCORD_BOT_TOKEN" \
  --set-string 'agents.kiro.discord.allowedChannels[0]=YOUR_CHANNEL_ID'
```

Or in a values file:

```yaml
podRuntimeClassName: kata-fc

agents:
  kiro:
    discord:
      botToken: "${DISCORD_BOT_TOKEN}"
      allowedChannels:
        - "YOUR_CHANNEL_ID"
```

Each agent pod now runs inside its own Firecracker microVM.

## Resource Overhead

Per microVM:
- **Memory**: ~30-50MB baseline (guest kernel + minimal userspace)
- **Startup**: ~125ms
- **Disk**: Shared read-only rootfs image, copy-on-write per VM

On an Intel N100 (8-16GB RAM) with K3s:
- K3s control plane: ~500MB
- Each OAB agent in Firecracker: ~100-200MB (microVM overhead + agent process)
- Comfortable capacity: 2-4 agents

## Limitations

- **No GPU passthrough** — Firecracker does not support GPU. Use `kata-qemu` if you need GPU access.
- **Block storage only** — Firecracker supports up to 7 block devices per VM. PVCs work fine.
- **No device hotplug** — Resources must be defined at VM creation time.
- **Linux only** — Firecracker requires KVM (Linux host).

## Comparison

| | runc (default) | kata-fc (Firecracker) | kata-qemu (QEMU) |
|---|---|---|---|
| Isolation | Container | microVM | microVM |
| Startup | ~50ms | ~125ms | ~300ms |
| Memory overhead | ~0 | ~30MB | ~50-100MB |
| GPU support | ✅ | ❌ | ✅ |
| Security | Namespace/cgroup | Independent kernel | Independent kernel |
| Best for | Trusted workloads | **Untrusted/AI agents** | GPU + isolation |

## Uninstall

```bash
# Remove kata-deploy
kubectl delete -k https://github.com/kata-containers/kata-containers/tools/packaging/kata-deploy/kata-deploy/overlays/k3s
kubectl -n kube-system wait --timeout=10m --for=delete -l name=kata-deploy pod

# Cleanup node labels and containerd config
kubectl apply -f https://raw.githubusercontent.com/kata-containers/kata-containers/main/tools/packaging/kata-deploy/kata-cleanup/base/kata-cleanup.yaml
# Wait ~5 minutes, then:
kubectl delete -f https://raw.githubusercontent.com/kata-containers/kata-containers/main/tools/packaging/kata-deploy/kata-cleanup/base/kata-cleanup.yaml
kubectl delete -f https://raw.githubusercontent.com/kata-containers/kata-containers/main/tools/packaging/kata-deploy/runtimeclasses/kata-runtimeClasses.yaml
```
