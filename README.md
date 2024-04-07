# WSL headless developer tools

This quick & dirty Rust project can be used to turn your old Windows machine into an SSH dev server.

It's one binary that does the following:

1. Prevents Windows sleep while it's running.
2. Starts an SSH port forwarder from `0.0.0.0:22` to WSL's port 2022 - redirecting SSH connections to an SSH server in WSL.
3. Launches a startup script (an SSH server) in WSL and automatically restarts it in case it crashes.

Stopping this tool (press <kbd>CTRL</kbd>+<kbd>C</kbd>) will shut down the SSH server and allow Windows to sleep.

## What might you need it for

* If you prefer your development machine and switching to another OS or keyboard layout is uncomfortable
* To borrow a Windows PC that's more powerful than your primary development machine (or has extra hardware)
  * e.g. to develop/run CUDA or x86 code from a lightweight ARM64 laptop
  * or to access a gaming PC from another area of your house for ML workloads
* If double-booting a real Linux OS or running a real Linux VM on the Windows machine is impractical

## Preparations

Install and configure WSL.

Generate SSH host keys:

```shell
sudo dnf --setopt=install_weak_deps-False install openssh-server
mkdir -p ~/.ssh/sshd
ssh-keygen -q -N "" -t dsa -f ~/.ssh/sshd/ssh_host_dsa_key
ssh-keygen -q -N "" -t rsa -b 4096 -f ~/.ssh/sshd/ssh_host_rsa_key
ssh-keygen -q -N "" -t ecdsa -f ~/.ssh/sshd/ssh_host_ecdsa_key
ssh-keygen -q -N "" -t ed25519 -f ~/.ssh/sshd/ssh_host_ed25519_key
sudo cat /etc/ssh/sshd_config > ~/.ssh/sshd/sshd_config
```

Then update paths to `HostKey` and the port (to `2022`) in `~/.ssh/sshd/sshd_config`.
Set PidFile to `~/.ssh/sshd.pid`.

To **temporarily** enable password authentication, set `PasswordAuthentication: yes` and `UsePAM: yes`.

## How to use it

Connect to your Windows machine using ssh, with the default port (22).

The [VSCode SSH extension](https://code.visualstudio.com/docs/remote/ssh) works well with this tool - extensions run in WSL, while VSCode just shows the UI.

# External dependencies

Borrowed Windows system code from https://github.com/curtisalexander/stay-awake2
