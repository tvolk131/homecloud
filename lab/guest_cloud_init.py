"""Single source for the lab's Ubuntu development guest bootstrap."""


def cloud_config(public_key):
    assert public_key.startswith('ssh-ed25519 ') and '\n' not in public_key
    return '''#cloud-config
users:
  - name: dev
    groups: [sudo]
    sudo: ALL=(ALL) NOPASSWD:ALL
    shell: /bin/bash
    lock_passwd: true
    ssh_authorized_keys:
      - ''' + public_key + '''
ssh_pwauth: false
disable_root: true
ssh_deletekeys: true
package_update: true
packages: [openssh-server, curl, git, ca-certificates]
write_files:
  - path: /etc/ssh/sshd_config
    owner: root:root
    permissions: '0644'
    content: |
      UsePAM yes
      PubkeyAuthentication yes
      PasswordAuthentication no
      KbdInteractiveAuthentication no
      PermitRootLogin no
      AuthorizedKeysFile .ssh/authorized_keys
      Subsystem sftp internal-sftp
      Include /etc/ssh/sshd_config.d/*.conf
runcmd:
  - [install, -d, -m, '0755', /run/sshd]
  - [sshd, -t]
  - [systemctl, enable, --now, ssh]
  - [systemctl, restart, ssh]
'''
