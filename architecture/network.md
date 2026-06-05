# Networking Subsystem — Implementation Specification

> Status: Draft v0.2 (build spec)
> Target: riscv64
> IPC model: capability handles + bidirectional channels (pipe semantics available)
> Stack core: smoltcp
> Audience: implementers. This document is meant to be sufficient to start writing code.

---

## 0. Conventions

- **Integers** are little-endian (riscv64 LE). Fixed-width types use Rust names: `u8 u16 u32 u64 i32`.
- **`Handle`** = `u32` index into the calling process's handle table. `0` is never valid (reserved as `HANDLE_INVALID`).
- **All multi-byte wire fields** in channel messages are LE and naturally aligned; structs are `#[repr(C)]`.
- **Network-order fields** (IP addresses, ports as they appear on the wire) are explicitly called out as `be16`/`be32`. Everything else is host order.
- **Error returns**: syscalls return `i64`; `>= 0` is success (often a `Handle` or byte count), `< 0` is `-errno` (see §11).
- **NIC byte buffers** are always whole L2 frames including the Ethernet header, excluding the FCS (the driver/hardware handles FCS).

---

## 1. Kernel Primitives Assumed

This spec builds on five kernel facilities. They are *assumed to exist*; their exact internals are out of scope, but their contracts are fixed here because the subsystem depends on them.

### 1.1 Handles and capability rights

A `Handle` references a kernel object and carries a **rights bitmask** (`u32`). Rights relevant to us:

```
RIGHT_READ      0x0001   // may read / recv
RIGHT_WRITE     0x0002   // may write / send
RIGHT_DUP       0x0004   // may be duplicated
RIGHT_TRANSFER  0x0008   // may be sent over a channel to another process
RIGHT_MANAGE    0x0010   // may issue control ops (bind, listen, setopt...)
RIGHT_REVOKE    0x0020   // holder may revoke derived handles
```

Rights can only ever be **narrowed** when a handle is duplicated or transferred, never widened.

### 1.2 Channel syscalls

A channel is a bidirectional message endpoint. Each message is `(bytes, [handles])` — a byte payload plus an ordered list of transferred handles (Fuchsia-style handle passing).

```
// Create a connected channel pair. Returns two handles via out-params.
sys_channel_create(out_a: *mut Handle, out_b: *mut Handle) -> i64

// Send one message. `handles` are MOVED out of the caller on success.
sys_channel_send(ch: Handle,
                 bytes: *const u8, nbytes: u32,
                 handles: *const Handle, nhandles: u32) -> i64

// Receive one message. Blocks unless O_NONBLOCK was set on `ch`.
// Returns number of bytes written. Received handles land in `out_handles`.
sys_channel_recv(ch: Handle,
                 bytes: *mut u8, byte_cap: u32,
                 out_handles: *mut Handle, handle_cap: u32,
                 out_nhandles: *mut u32) -> i64
```

Semantics:
- Messages are **datagram-framed**: one send = one recv, never coalesced or split.
- If `byte_cap` is too small the call returns `-EMSGSIZE` and the message stays queued (peek the size with a `recv` of `byte_cap=0`, which returns the needed size as `-EMSGSIZE` is *not* used here — instead returns the size as a positive number with no dequeue when a `MSG_PEEK`-style flag is set; see note).
- Closing one end causes the peer's `recv` to drain remaining queued messages, then return `-EPIPE` (pipe semantics).

> **Pipe semantics note:** a channel with no handles ever attached and used purely with `recv`/`send` of bytes degrades to an ordered byte-message pipe, which is what we use for bulk data paths (§5, §6). Control paths use the handle-passing capability.

### 1.3 Shared memory objects (VMOs)

```
sys_vmo_create(size: u64, out: *mut Handle) -> i64
sys_vmo_map(vmo: Handle, out_vaddr: *mut usize) -> i64     // maps into caller
sys_vmo_size(vmo: Handle, out: *mut u64) -> i64
```

A VMO handle can be transferred over a channel; the receiver maps it to share memory. Used for the zero-copy DMA path (§5.4) and for `recd` checkpoint regions (§8.3).

### 1.4 Interrupt objects

```
sys_irq_create(irq_num: u32, out: *mut Handle) -> i64   // privileged; granted to nicd at spawn
sys_irq_wait(irq: Handle) -> i64                          // blocks until IRQ fires; returns count
sys_irq_ack(irq: Handle) -> i64
```

### 1.5 MMIO / DMA grants

```
sys_mmio_map(phys_base: u64, len: u64, out_vaddr: *mut usize) -> i64  // privileged
sys_dma_alloc(len: u64, out_vaddr: *mut usize, out_phys: *mut u64) -> i64
```

Both are granted to `nicd` at spawn via its capability manifest and to no one else. A compromised `nicd` therefore cannot map another device's registers.

---

## 2. Process & Capability Topology

```
recd (supervisor, trusted)
 ├─ spawns nicd@eth0   grants: {mmio(eth0), irq(eth0), dma}
 ├─ spawns netd        grants: {channel→nicd(control), checkpoint VMO}
 ├─ spawns cfgd        grants: {channel→netd(control, RIGHT_MANAGE)}
 └─ holds health-ping channel to each child

Application
 └─ receives a NET capability handle from its launcher (env handle slot 3)
    This handle is a channel to netd's "open" port, with rights narrowed
    to what the launcher permits (e.g. RIGHT_WRITE|RIGHT_READ, no RIGHT_MANAGE).
```

A process with **no** NET handle in its handle table cannot reach the network at all — there is no ambient/global network namespace to fall back on.

---

## 3. Naming

Resources are named `scheme:reference`, resolved by sending an **OPEN** control message (§4.2) to a scheme provider over a channel. The name selects *what*; the channel handle's rights gate *whether*.

| Name | Provider | Meaning |
|---|---|---|
| `link:eth0` | nicd | raw L2 frame endpoint for one NIC |
| `net:tcp/<ip>:<port>` | netd | outbound TCP connection target, or `0.0.0.0:<port>` for a listener |
| `net:udp/<ip>:<port>` | netd | UDP socket bound to local `<ip>:<port>` |
| `net:icmp/<ip>` | netd | ICMP echo endpoint (ping w/o privilege) |
| `net:cfg/addr` `net:cfg/route` `net:cfg/dns` | netd | configuration nodes (require RIGHT_MANAGE) |

Reference grammar (resolved by the provider, not the kernel):

```
ipv4   = dec-octet "." dec-octet "." dec-octet "." dec-octet
ipv6   = <RFC 4291 textual form, bracketed when followed by port>
port   = 1*5DIGIT            ; 0 = "any" for binds
host   = ipv4 / "[" ipv6 "]"
target = host ":" port
```

---

## 4. Control Protocol (channel messages)

All control messages share a header. The first `u16` of every control byte-payload is an opcode.

### 4.1 Header

```rust
#[repr(C)]
struct CtrlHeader {
    opcode: u16,   // see table below
    flags:  u16,
    txid:   u32,   // request id; reply echoes it
    len:    u32,   // length of the body that follows this header, in bytes
}
```

| Opcode | Name | Direction | Body |
|---|---|---|---|
| 0x0001 | OPEN | app→provider | `OpenReq` |
| 0x0002 | OPEN_REPLY | provider→app | `OpenReply` (+ 1 transferred handle on success) |
| 0x0003 | BIND | app→netd | `BindReq` |
| 0x0004 | LISTEN | app→netd | `ListenReq` |
| 0x0005 | ACCEPT | app→netd | `AcceptReq` |
| 0x0006 | ACCEPT_REPLY | netd→app | `AcceptReply` (+ 1 transferred conn handle) |
| 0x0007 | CONNECT_STATUS | netd→app | `ConnStatus` (async; connection established/failed) |
| 0x0008 | SETOPT | app→netd | `SetOptReq` |
| 0x0009 | GETOPT | app→netd | `GetOptReq` / reply |
| 0x000A | CLOSE | app→provider | `CloseReq` |
| 0x000B | SENDMSG | app→netd | datagram send w/ endpoint (§4.4) |
| 0x000C | RECVMSG | app→netd | datagram recv w/ endpoint (§4.4) |
| 0x0100 | HEALTH_PING | recd→child | empty |
| 0x0101 | HEALTH_PONG | child→recd | `HealthPong` |
| 0x7FFF | ERROR | provider→app | `ErrorReply` (errno + context) |

### 4.2 OPEN

```rust
#[repr(C)]
struct OpenReq {
    hdr: CtrlHeader,
    name_len: u16,     // bytes of UTF-8 name following this struct
    want_rights: u32,  // requested rights; provider clamps to ≤ channel rights
    // followed by name_len bytes of "scheme:reference"
}

#[repr(C)]
struct OpenReply {
    hdr: CtrlHeader,   // opcode=OPEN_REPLY, txid echoed
    status: i32,       // 0 = ok, else -errno; on ok a handle is transferred
    granted_rights: u32,
}
```

On success the reply message carries exactly one transferred handle: a **per-resource channel** to that socket/connection. All subsequent data and control for that resource go over the returned handle. This makes each socket an independently-revocable capability.

### 4.3 Endpoint struct (used everywhere an address appears)

```rust
#[repr(C)]
struct Endpoint {
    family: u16,      // 1 = IPv4, 2 = IPv6
    port:   be16,     // network order
    addr:   [u8; 16], // IPv4 in first 4 bytes (rest zero), IPv6 full 16
}
```

### 4.4 Datagram verbs (the Redox lesson, designed in)

`read`/`write` carry no address, so datagrams use dedicated verbs from the start.

```rust
#[repr(C)]
struct SendMsgReq {
    hdr: CtrlHeader,        // opcode=SENDMSG
    to:  Endpoint,
    payload_len: u32,
    // followed by payload_len bytes
}

#[repr(C)]
struct RecvMsgReply {
    hdr: CtrlHeader,        // opcode=RECVMSG
    from: Endpoint,
    payload_len: u32,
    // followed by payload_len bytes
}
```

Stream sockets (`net:tcp/...`) use plain channel byte-messages on the per-resource handle (no per-message endpoint needed) — i.e. the pipe degradation from §1.2.

---

## 5. `nicd` — NIC Driver

### 5.1 Responsibilities

- Drive one device; expose `link:<dev>`.
- Translate between hardware descriptor rings and the `netd`-facing frame channel.
- Treat hardware as the only thing below it and `netd` as untrusted above it (validate lengths/offsets it’s told to DMA).

### 5.2 Spawn contract

`recd` spawns `nicd` with a handle manifest:

```
slot 1: control channel  (to netd, RIGHT_READ|WRITE)
slot 2: irq object        (RIGHT_READ)
slot 3: mmio mapping for device registers
slot 4: dma pool          (pre-allocated VMO, mapped)
slot 5: health channel    (to recd)
```

### 5.3 Frame path — v1 (copying, correct-first)

Two threads:

- **RX thread**: `sys_irq_wait` → drain hardware RX ring → for each frame, `sys_channel_send(netd_data_ch, frame_bytes, [])`.
- **TX thread**: `sys_channel_recv(netd_data_ch, ...)` → copy into a TX descriptor → kick hardware → on completion, free descriptor.

This intentionally accepts the copy + the historically-known transmit blocking cost. Correctness ships first.

### 5.4 Frame path — v2 (zero-copy, behind same interface)

The data channel’s first message at link bring-up may carry a **shared DMA VMO handle**. When present, `netd` writes frames directly into that region and sends only `(offset,len)` descriptors over the channel; `nicd` programs the descriptor’s physical address into the ring with no copy. The control opcode set does not change — v2 is a negotiated capability, not an ABI break.

```rust
#[repr(C)]
struct FrameDesc {   // sent over data channel in v2 mode
    offset: u32,     // into the shared VMO
    len:    u32,
    flags:  u32,     // bit0 = end-of-batch
}
```

### 5.5 `link:` control ops

| Op | Effect |
|---|---|
| OPEN `link:eth0` | returns per-link data channel handle (+ optional DMA VMO) |
| SETOPT `promisc` | enable promiscuous mode (requires RIGHT_MANAGE) |
| GETOPT `mac` | returns 6-byte MAC |
| GETOPT `mtu` | returns u16 MTU |
| GETOPT `link_up` | returns u8 carrier state |

---

## 6. `netd` — Protocol Daemon (smoltcp integration)

### 6.1 Internal structure

```
netd
 ├─ poll loop (single-threaded, event-driven)
 ├─ smoltcp Interface  (one per link device)
 ├─ SocketSet          (smoltcp sockets)
 ├─ table: SocketId -> { smoltcp handle, owner channel, type, state }
 └─ control dispatch   (handles OPEN/BIND/.../SENDMSG/RECVMSG)
```

smoltcp gives us the TCP/UDP/ICMP/ARP state machines; `netd` is the glue between smoltcp’s `phy::Device` trait and the `nicd` data channel, plus the mapping from our scheme/channel world to smoltcp sockets.

### 6.2 The `phy::Device` shim

Implement smoltcp’s `Device`/`RxToken`/`TxToken` over the `nicd` data channel:

```rust
struct NicdDevice {
    data_ch: Handle,
    mtu: usize,
    // v2: dma: Option<DmaPool>,
}

impl phy::Device for NicdDevice {
    // receive(): non-blocking channel recv of one frame -> RxToken wrapping the bytes
    // transmit(): TxToken whose consume() builds a frame and channel-sends it to nicd
    fn capabilities(&self) -> DeviceCapabilities { /* mtu, checksum offload = none in v1 */ }
}
```

### 6.3 Poll loop

```
loop {
    timestamp = now();
    iface.poll(timestamp, &mut device, &mut sockets);   // smoltcp advances all sockets

    drain_control_channels();   // OPEN/BIND/CONNECT/SENDMSG/etc from app handles
    pump_socket_data();         // move bytes between smoltcp sockets and per-socket channels
    answer_health_ping();       // respond to recd if pinged

    delay = iface.poll_delay(timestamp, &sockets);   // smoltcp tells us max sleep
    wait_until_event_or(delay); // wake on: nicd frame, any app channel, timer, health ping
}
```

The wait multiplexes: NIC data channel readiness, all app/control channel readiness, and the smoltcp timer. On riscv64 this is a `sys_object_wait_many(handles, timeout)` (a kernel multi-wait; assumed in §1 family).

### 6.4 TCP connect flow (concrete)

```
app: OPEN "net:tcp/93.184.216.34:80", want_rights=READ|WRITE
netd:
   - validate caller channel has READ|WRITE
   - create smoltcp tcp socket; socket.connect(remote, ephemeral_local)
   - allocate SocketId; create channel pair (a,b)
   - keep `a`; reply OPEN_REPLY{status=0} transferring `b` to app
   - SYN goes out next poll
   - on smoltcp state -> ESTABLISHED: send CONNECT_STATUS{ok} over the socket channel
app:
   - writes request bytes via sys_channel_send on its socket handle
netd:
   - pump_socket_data copies channel bytes into smoltcp tx buffer
   - smoltcp segments + sends via NicdDevice::transmit -> nicd -> wire
   - inbound data: smoltcp rx buffer -> channel-send to app
close: app sends CLOSE (or drops handle -> channel EPIPE) -> netd issues smoltcp close()
```

### 6.5 UDP flow

```
app: OPEN "net:udp/0.0.0.0:5353"  -> per-socket channel handle
app: SENDMSG{to=Endpoint, payload} -> netd: smoltcp udp socket.send_slice(payload, to)
wire inbound: smoltcp udp recv -> netd: RECVMSG reply{from, payload} to app
```

### 6.6 Socket table entry

```rust
struct SockEntry {
    id: SocketId,            // u32
    kind: SockKind,          // Tcp | Udp | Icmp
    smol: SocketHandle,      // smoltcp handle
    chan: Handle,            // netd's end of the per-socket channel
    rights: u32,             // effective rights granted to the app end
    state: SockState,        // mirror for fast queries / recovery checkpoint
}
```

### 6.7 Layer toggles (MINIX touch)

Boot config may disable layers: `ip=off` disables tcp+udp; `tcp=off`/`udp=off` independently. A disabled layer rejects OPEN of its scheme with `-EPROTONOSUPPORT`. `link:`-only operation (L2 listen) remains available with `ip=off`.

---

## 7. `cfgd` — Configuration & DHCP

- Owns DHCP client state machine (DISCOVER/OFFER/REQUEST/ACK, lease renew at T1/T2).
- Pushes results to netd via `net:cfg/*` with a RIGHT_MANAGE handle:
    - `net:cfg/addr`  — set interface IP/prefix
    - `net:cfg/route` — add/delete routes (default gw)
    - `net:cfg/dns`   — resolver list (consumed by libc resolver)
- Kept out of netd so the protocol core stays restartable independently; a cfgd crash never drops live connections.

DHCP itself runs as a normal UDP client through netd (`net:udp/0.0.0.0:68`), i.e. cfgd is just another app from netd’s perspective, holding an elevated cfg handle in addition.

---

## 8. Fault Recovery (`recd`)

### 8.1 Health protocol

`recd` sends HEALTH_PING every `T_ping` (default 1s) on each child’s health channel. A child must answer HEALTH_PONG within `T_timeout` (default 3 × T_ping).

```rust
#[repr(C)]
struct HealthPong {
    hdr: CtrlHeader,    // opcode=HEALTH_PONG
    seq: u32,           // echoes ping seq
    busy_hint: u32,     // optional: queued work depth, for backpressure telemetry
}
```

Failure triggers: missed pongs, process fault notification (kernel sends a `CHILD_FAULTED` message on the spawn channel), or malformed control replies.

### 8.2 Restart sequence

```
1. recd: sys_handle_revoke_derived(child_root)  // kernel revokes everything the child issued
2. recd: sys_proc_kill(child); reap
3. recd: re-spawn from the same manifest (§5.2 / equivalent for netd, cfgd)
4. recd: re-grant capability set
5. recd: notify dependents to re-attach (netd re-opens link:; apps see socket EPIPE)
```

Revocation in step 1 is what stops a half-dead netd from continuing to drive nicd: nicd’s next op on the revoked data channel returns `-EPIPE`.

### 8.3 netd state recovery — two tiers

- **Tier 1 (v1):** connections drop. Each app socket channel returns `-ECONNRESET` on next op; apps reconnect. Simple and honest.
- **Tier 2 (later):** netd checkpoints socket *metadata* (not payload) into a shared VMO owned by recd:

```rust
#[repr(C)]
struct SockCheckpoint {
    id: u32,
    kind: u8,
    _pad: [u8;3],
    local: Endpoint,
    remote: Endpoint,
    tcp_snd_nxt: u32,
    tcp_rcv_nxt: u32,
    tcp_state: u8,    // smoltcp TCP state enum, serialized
    _pad2: [u8;3],
}
```
On reincarnation the fresh netd maps the VMO, and for each ESTABLISHED checkpoint reconstructs a smoltcp socket pre-seeded with the sequence numbers. Connections whose peers stayed quiet during the gap survive. This is best-effort and gated behind a build flag.

---

## 9. Application Library (`libnet`) and POSIX shim

### 9.1 libnet (native, capability-aware)

```rust
pub struct NetCap(Handle);            // the channel to netd's open port
pub struct TcpStream(Handle);         // per-socket channel
pub struct UdpSocket(Handle);

impl NetCap {
    pub fn from_env() -> Result<Self>;             // reads handle slot 3
    pub fn tcp_connect(&self, target: &str) -> Result<TcpStream>;
    pub fn tcp_listen(&self, bind: &str) -> Result<TcpListener>;
    pub fn udp_bind(&self, bind: &str) -> Result<UdpSocket>;
}
impl TcpStream { pub fn read(&self,&mut[u8])->Result<usize>; pub fn write(&self,&[u8])->Result<usize>; }
impl UdpSocket { pub fn send_to(&self,&[u8],&Endpoint)->Result<usize>; pub fn recv_from(&self,&mut[u8])->Result<(usize,Endpoint)>; }
```

### 9.2 POSIX shim (in libc)

Map BSD sockets onto the above. The shim must **never widen** access: it can only exercise the NetCap the process already holds; calls exceeding granted scope return `EACCES`.

| POSIX call | Mapping |
|---|---|
| `socket(AF_INET, SOCK_STREAM, 0)` | allocate a deferred-target slot, no channel yet |
| `connect(fd, sa)` | OPEN `net:tcp/<sa>`, store returned handle as fd backing |
| `send/recv` | channel send/recv on the socket handle |
| `bind`+`listen`+`accept` | BIND/LISTEN/ACCEPT control ops; ACCEPT_REPLY transfers new conn handle = new fd |
| `sendto/recvfrom` | SENDMSG/RECVMSG (§4.4) |
| `setsockopt` | SETOPT |
| `close` | CLOSE / drop handle |

fd→Handle mapping lives in the libc process; the kernel sees only handles.

---

## 10. Wire/Frame Validation Rules (security-critical)

netd MUST, before trusting any frame from nicd:
- reject frames `< 14` bytes (no Ethernet header) or `> MTU+18`.
- bound-check every length/offset in v2 FrameDesc against the shared VMO size.
- never index smoltcp buffers with attacker-derived lengths unchecked.

nicd MUST, before DMA from a netd-supplied descriptor (v2):
- verify `offset + len <= vmo_size` and `len <= MTU`.

These checks are the practical content of "drivers are untrusted."

---

## 11. Error Codes

Subset of POSIX errno used on the `i64` return path; negative.

| Val | Name | Meaning here |
|---|---|---|
| 1 | EPERM | rights bitmask insufficient |
| 9 | EBADF | bad handle |
| 13 | EACCES | POSIX shim would widen scope |
| 32 | EPIPE | peer channel closed (component died / handle revoked) |
| 90 | EMSGSIZE | recv buffer too small |
| 92 | EPROTONOSUPPORT | layer disabled (§6.7) or scheme unknown |
| 104 | ECONNRESET | connection dropped (netd restart, Tier 1) |
| 111 | ECONNREFUSED | RST received on connect |
| 110 | ETIMEDOUT | connect/handshake timeout |
| 11 | EAGAIN | non-blocking op would block |
| 22 | EINVAL | malformed control body / bad name |

---

## 12. Build Phases (with concrete exit criteria)

| Phase | Deliverable | Exit criterion (testable) |
|---|---|---|
| P0 | kernel multi-wait + handle revoke verified | unit test: revoke makes peer recv return EPIPE |
| P1 | nicd@eth0 (qemu virtio-net or e1000), v1 copying path, `link:` OPEN/GETOPT | tcpdump on host sees frames netd→wire via a raw send test |
| P2 | netd: NicdDevice shim + smoltcp poll loop; TCP connect + UDP | `net:tcp` GET to a host server returns bytes; `net:udp` DNS query round-trips |
| P3 | libnet + libc POSIX shim | unmodified `curl`-class client fetches over the stack |
| P4 | recd: health ping, Tier-1 restart, revoke-on-death | kill nicd mid-transfer; new nicd spawns; new TCP connect succeeds within T_timeout |
| P5 | cfgd: DHCP client, routes, DNS push; IPv6 host | qemu DHCP lease obtained; default route installed; ICMPv6 ND works |
| P6 | v2 zero-copy DMA descriptors; Tier-2 checkpoint recovery | throughput test shows no per-frame copy; netd restart preserves an idle ESTABLISHED conn |

---

## 13. riscv64 Notes

- **Endianness**: riscv64 is LE; matches §0. No byte-swapping for host fields; use explicit `to_be`/`from_be` only for `be16`/`be32` wire fields.
- **MMIO**: device register access via `sys_mmio_map`; use `read_volatile`/`write_volatile`. Honor the RISC-V memory model — insert `fence` (e.g. `fence o,i` patterns) between descriptor writes and the doorbell write so the device sees ordered updates.
- **DMA coherence**: assume non-coherent DMA is possible on some riscv64 SoCs; the DMA pool path must issue cache maintenance (clean before TX, invalidate after RX) where the platform requires it. On a coherent platform (qemu `virt`) these are no-ops.
- **IRQ**: PLIC-routed external interrupts surface through `sys_irq_wait`; ack via `sys_irq_ack` which the kernel translates to the PLIC completion.
- **Atomics/`fence`**: the poll loop is single-threaded so smoltcp needs no locking; the only cross-thread sharing in nicd (RX vs TX thread) uses channel sends, not shared mutable state, avoiding explicit barriers beyond the MMIO fences above.

---

## 14. Open Items (carried from design, now scoped)

1. **Tier-2 checkpoint cadence** — checkpoint on state transition vs fixed interval; transition-driven is cheaper and chosen as default, interval as fallback for long-lived idle conns.
2. **Capability scoping expressiveness** — v1 limits scope to rights bits + (optional) a single allowed remote prefix per NetCap; richer filtering deferred to avoid building a firewall inside the cap layer.
3. **Loopback** — implement as a dedicated `link:lo` device inside netd (a virtual `phy::Device` that loops frames) rather than special-casing, sidestepping smoltcp’s historical ARP-flood-on-127.0.0.1 issue.
4. **Backpressure** — when an app stops reading, its per-socket channel fills; netd stops draining that smoltcp socket’s rx buffer, which closes the TCP window naturally. Drop policy for UDP: tail-drop at channel depth N (config).
5. **Multi-NIC** — table already keys interfaces per device; routing across them deferred to post-P5.