# R2 runbook: preparing the two Macs (PROPOSED, not approved)

**Status: PROPOSED.** One-time setup for the
[R2 two-Mac variant](feeder-under-load.md#r2-two-mac-variant-proposed-not-approved)
of the feeder-under-load protocol. Nothing here has been done; the
maintainer performs it, and only after approving the variant and confirming
that both Macs are available. It changes nothing in the repository.

Host K is the Apple M3 that runs Knotrel; host L is the MacBook Pro M4 that
runs PostgreSQL, the feeder and the load tool. Every arm is driven from host
L; host L connects to host K over SSH, never the other way.

## Host L (load)

1. **Toolchain and checkout.** Rust 1.100.0 (`rustup toolchain install
   1.100.0`), Git, Docker Desktop. Clone the repository to
   `$HOME/knotrel-r2/knotrel` and check out the commit that adds the R2
   variant (record it).
2. **Build the feeder and the load tool:**

   ```bash
   cd "$HOME/knotrel-r2/knotrel/integrations/postgres-feeder"
   cargo build --release --locked --bins --example load
   shasum -a 256 target/release/knotrel-pg-feeder target/release/examples/load
   ```

3. **PostgreSQL container.** Start Docker Desktop, then check that the
   pinned 18.7 image starts and stops (this pulls it once):

   ```bash
   scripts/start-test-postgres.sh 18 && scripts/stop-test-postgres.sh 18
   ```

   During arms T and U the container runs on host L only; the arm commands
   start and stop it.
4. **Link.** Connect the two Macs with a Thunderbolt or Ethernet cable and
   note host K's address on that interface as `KADDR` (System Settings →
   Network → Thunderbolt Bridge, or `ifconfig bridge0` on host K). Wi-Fi may
   stay on for other traffic, but `route get KADDR` must name the wired
   interface.
5. **SSH key for host K.** A dedicated key and an alias that goes over the
   link:

   ```bash
   ssh-keygen -t ed25519 -f ~/.ssh/knotrel_r2 -C knotrel-r2
   ssh-copy-id -i ~/.ssh/knotrel_r2.pub <user on K>@KADDR
   ```

   and in `~/.ssh/config`:

   ```text
   Host hostk
     HostName KADDR
     User <user on K>
     IdentityFile ~/.ssh/knotrel_r2
     IdentitiesOnly yes
   ```

   Check that it needs no prompt (the tool uses `BatchMode=yes`):
   `ssh -o BatchMode=yes hostk 'sysctl -n hw.ncpu'` prints 8.

## Host K (Knotrel)

1. **Remote Login** on (System Settings → General → Sharing → Remote
   Login), limited to the maintainer's user.
2. **Server under test**, at the same path layout as host L:

   ```bash
   git -C "$HOME/knotrel-r2/knotrel" worktree add "$HOME/knotrel-r2/knotrel-v0.9.0" v0.9.0
   cd "$HOME/knotrel-r2/knotrel-v0.9.0" && cargo build -p knotrel-server --release --locked
   shasum -a 256 target/release/knotrel-server
   ```

3. **Feeder and load tool** at the same commit as host L, built the same way
   (arm S runs them on host K; arm U uses the load binary as its busy loop).
4. **PostgreSQL image** pulled for arm S (`scripts/start-test-postgres.sh 18
   && scripts/stop-test-postgres.sh 18` with Docker Desktop running). Docker
   Desktop is quit after arm S.
5. **Firewall.** Either the macOS firewall is off on host K, or
   `knotrel-server` is allowed to accept incoming connections: start it once
   bound to `KADDR` and answer the dialog, so that no dialog is pending on
   host K's screen during the session.

## Before the session

Go through the pre-flight checklist of the variant. Then follow its
commands in order S, T, U, and the decision, in one session.
