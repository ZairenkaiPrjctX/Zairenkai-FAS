# Project context: Zairenkai-FAS

> Owned by this repository. Update it whenever verified facts change. Label every statement
> **Verified** (with `Evidence:`), **Unverified**, or **Unknown / Not Configured**.

## Identity

| Field | Value | Status |
|---|---|---|
| Repository | `ZairenkaiPrjctX/Zairenkai-FAS` | Verified. Evidence: GitHub API |
| GitHub description | Zairenkai Tweaks FAS kernel module | Verified. Evidence: GitHub API |
| Visibility | private | Verified. Evidence: GitHub API |
| Configured default branch | `main` (no commits exist on it yet) | Verified. Evidence: GitHub API |
| Purpose | FAS implementation and its integration interfaces (description says kernel module). | **Unverified**: rollout hint, not derived from code |

## Contents

**Unknown / Not Configured.** On 2026-10-18 the repository was empty. Evidence: GitHub API 2026-10-18: repository size 0, no branches, no commits; ls-remote returned no refs.
The only files present are the agent foundation itself.

## Key components

Unknown. Nothing to list until source code exists.

## Build, test, and run

Not Configured. No manifests, build scripts, or CI workflows exist.

## Authoritative sources

| Topic | Source |
|---|---|
| Repository facts | this file |
| Architecture | `AGENT/ARCHITECTURE.md` |
| Organization standards | `ZairenkaiPrjctX/.github` → `agent-foundation/standards/` |
| Repository list and relationships | `ZairenkaiPrjctX/.github` → `agent-foundation/inventory/repositories.json` |

## Related repositories

None verified. Record relationships here, with evidence, once code shows them.

## Open questions to answer when code lands

- The description says 'FAS kernel module': which kernel versions and build system (Kbuild, DKMS, vendor tree) are targeted?
- What userspace interface (sysfs, procfs, ioctl) does it expose, and who consumes it?
