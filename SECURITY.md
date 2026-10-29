# Security policy

## Reporting a vulnerability

Please report security problems privately through GitHub's [private vulnerability reporting](https://github.com/cyenite/annex/security/advisories/new), not in a public issue. Include what an attacker can do, the steps to reproduce, and the annex version (`annex --version`).

You can expect an acknowledgement within a week. Once a fix is ready, it ships in a release with an advisory crediting you, unless you'd rather stay anonymous.

## Threat model

annex is a developer tool for a single-user workstation. What it defends:

- **Other local users must not reach an instance.** Redroid containers run privileged, so a shell in one is root on the host. adbd only accepts the key in `~/.android/adbkey.pub`, keys saved on the data volume are masked, and no other service in the container listens on the network.
- **Snapshots are private.** They hold every app's data, including login tokens, so their folders are mode 0700 and archives 0600.
- **Clipboard contents are never logged.**

What it does not defend against:

- **Members of the `docker` group.** They can `docker exec` into any container, which is inherent to Docker.
- **Processes running as you.** They can use your adb key, like any adb client.
- **Apps inside an instance.** Treat an instance like a test device: don't install untrusted apps on one that holds real credentials.
- **Waydroid's own services.** Waydroid's container service accepts freeze, unfreeze and stop calls from any local user over the system bus. annex uses those calls but does not change Waydroid's policy.

## Supported versions

Security fixes go into the latest release only.
