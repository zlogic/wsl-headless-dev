# WSL headless developer tools

This Rust project can be used to turn your old Windows machine into a VSCode dev server.

1. Disables Windows sleep.
2. Starts an SSH port forwarder - redirecting SSH connections to an SSH server in WSL.
3. Launches a startup script (an SSH server) in WSL and automatically restarts it in case it crashes.

## Preparations

Install and configure WSL.

Generate SSH host keys:

```shell
mkdir -p ~/.ssh/sshd
ssh-keygen -q -N "" -t dsa -f ~/.ssh/sshd/ssh_host_dsa_key
ssh-keygen -q -N "" -t rsa -b 4096 -f ~/.ssh/sshd/ssh_host_rsa_key
ssh-keygen -q -N "" -t ecdsa -f ~/.ssh/sshd/ssh_host_ecdsa_key
ssh-keygen -q -N "" -t ed25519 -f ~/.ssh/sshd/ssh_host_ed25519_key
cp /etc/ssh/sshd_config ~/.ssh/sshd
```

## Optional extras

Install [VSCode Server](https://code.visualstudio.com/docs/remote/vscode-server) into WSL.
Launch `vscode tunnel` once to configure VSCode tunneling and link it with your Github account.

# External dependencies

Borrowed Windows system code from https://github.com/curtisalexander/stay-awake2
