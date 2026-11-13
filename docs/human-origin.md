# Human origin: config, permission and control-registry writes need a human's signature

Some files decide what agents may do. The harness writes the config, the permission set and the control registry only when the write comes with a human approval. This page is the proof, the setup and the recipe.

## The proof

A detached OpenSSH signature (`ssh-keygen -Y sign`, namespace `bridlework-human-origin`) over a message the harness builds from the exact write:

```
bridlework-human-origin v1
target=config|permission|control_registry
name=<target file name, last component only>
expires_ms=<n>
content-follows
<the exact new content bytes>
```

The harness checks it with `/usr/bin/ssh-keygen -Y verify` (principal `bridlework-human`) against the trust anchor `/etc/bridlework/human-origin.allowed_signers`. The path is a constant: no flag and no env var can change it in a release build. The anchor must be a regular file (not a symlink), and the file and every folder above it must be owned by root with no group-write and no other-write bit. A missing or untrusted anchor is a refusal, never "no check".

The approval must not be expired, and its window (`expires_ms` minus now) is at most one hour.

An agent runs as the normal user. It cannot sign (no private key), cannot put its own key on the anchor (root-only), and cannot reuse an approval for other content (the content is inside the signed message). The human key should need a human: either a FIDO `ed25519-sk` key (a physical touch per signature; recommended) or a passphrase key that is never loaded into an ssh-agent an agent can reach. That is a trust assumption, not something the code checks.

## Install the anchor (the operator, once, with sudo; the harness never writes `/etc`)

```
sudo install -d -m 0755 -o root -g wheel /etc/bridlework
echo "bridlework-human namespaces=\"bridlework-human-origin\" $(cat ~/.ssh/id_ed25519_sk.pub)" | sudo tee /etc/bridlework/human-origin.allowed_signers
sudo chown root /etc/bridlework/human-origin.allowed_signers
sudo chmod 0644 /etc/bridlework/human-origin.allowed_signers
```

(On Linux use group `root`.) Use the release build of `harness`: a debug build has a test-only anchor flag.

## Sign and write

```
EXP=$(( ($(date +%s) + 600) * 1000 ))
harness config approval-message --config <config> --from <new-file> --expires-ms $EXP > msg
ssh-keygen -Y sign -n bridlework-human-origin -f <your-key> msg      # writes msg.sig
harness config write --config <config> --from <new-file> --approval msg.sig --expires-ms $EXP
```

The same flow covers the other two targets, through the same guarded write (`handback::guarded_write`, the only caller of the check):

```
harness controls approval-message --registry <controls/registry.json> --from <new-file> --expires-ms $EXP > msg
ssh-keygen -Y sign -n bridlework-human-origin -f <your-key> msg
harness controls write --registry <controls/registry.json> --from <new-file> --approval msg.sig --expires-ms $EXP

harness permission approval-message --settings <settings-file> --from <new-file> --expires-ms $EXP > msg
ssh-keygen -Y sign -n bridlework-human-origin -f <your-key> msg
harness permission write --settings <settings-file> --from <new-file> --approval msg.sig --expires-ms $EXP
```

`--registry` and `--settings` are required (no env default). New registry content must pass the strict registry parser; new permission content must be a JSON object whose only key is `permissions`, whose keys are a subset of `allow`, `deny`, `ask`, each an array of distinct non-empty strings. An approval signed for one target is refused on another (the target is inside the signed message).

`--log <file>` and `--root <dir>` choose the event log (default `.harness/events.jsonl`).

## Exit codes

- 0: written (`config written name=<name>`, `controls written name=<name>` or `permission written name=<name>`). An accepted write also logs a `human_origin_accepted` event (`target`, `path`). The line is appended after the new file is staged and before it is renamed into place, so no write lands unlogged. If the rename itself fails the log holds an accepted line with no landed write: the accepted line records that the approval was accepted, and the stdout `written` line confirms the write landed.
- 2: usage error, bad new content (it must pass the target's own rules: `harness config check` for the config, the registry parser, the permission-shape check), or the refusal could not be logged. Nothing is written.
- 3: refused. The file is untouched and a `human_origin_refused` event is logged. Stderr: `harness: human_origin_refused target=config|permission|control_registry reason=<word>`.

## Refusal words

`anchor_untrusted`, `no_approval`, `expired`, `window_too_long`, `signature_invalid`, `verifier_unavailable`.

## Not covered

A direct shell edit of `controls/registry.json` or of a permission file is not seen by these verbs (tracked). The live machine-wide managed-settings file is root-owned, so the operator installs it with sudo; the permission verb writes the repo's or overlay's permission set. An agent editing the config file directly with a shell is not seen by this verb; nor is replacing the `harness` binary. Both are tracked as follow-ups.
