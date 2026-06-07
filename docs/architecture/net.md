# 9. Networking

### 9.1 Architecture Overview

```
┌──────────────────────────────────────────────────┐
│                  User Space                       │
│                                                   │
│  Application ─► sys_connect/bind/send/recv        │
│       │                                           │
│       ▼  (kernel channels)                        │
│  net-smoltcp daemon                               │
│       │  smoltcp TCP/IP stack                     │
│       │  sys_net_send / sys_net_recv              │
└───────┼──────────────────────────────────────────┘
        │  (raw Ethernet frames)
┌───────┼──────────────────────────────────────────┐
│  Kernel                                           │
│       ▼                                           │
│  virtio-mmio NIC driver                           │
│       │  MMIO registers + descriptor rings        │
│       ▼                                           │
│  QEMU virtio-net device                           │
└──────────────────────────────────────────────────┘
```

The kernel provides **raw Ethernet frame I/O** only. All protocol processing (ARP, IP, TCP, UDP) lives in the `net-smoltcp` userspace daemon. This is the microkernel pattern applied to networking.

### 9.2 Virtio-mmio Driver

The virtio-net device is accessed via the **legacy virtio-mmio interface** (version 1):

**MMIO Register Map (offset from device base):**

| Offset | Register          | Description                         |
|--------|-------------------|-------------------------------------|
| 0x000  | `MagicValue`      | Must read `0x74726976` ("virt")     |
| 0x004  | `Version`         | Must be 1 (legacy)                  |
| 0x008  | `DeviceID`        | 1 = network device                  |
| 0x00C  | `VendorID`        | `0x554D4551` ("QEMU")               |
| 0x010  | `DeviceFeatures`  | Device-supported features bitmask   |
| 0x020  | `DriverFeatures`  | Features driver wishes to negotiate |
| 0x028  | `GuestPageSize`   | Must write 4096                     |
| 0x030  | `QueueSel`        | Select which virtqueue to configure |
| 0x034  | `QueueNumMax`     | Maximum queue size (read-only)      |
| 0x038  | `QueueNum`        | Set queue size                      |
| 0x03C  | `QueueAlign`      | Queue alignment                     |
| 0x040  | `QueuePFN`        | Queue physical page number          |
| 0x050  | `QueueNotify`     | Write queue index to trigger device |
| 0x060  | `InterruptStatus` | Bit 0: used buffer notification     |
| 0x064  | `InterruptACK`    | Write to acknowledge interrupt      |
| 0x070  | `Status`          | Driver status bits                  |

**Initialization sequence:**

```
1. Write Status = 0                    (reset)
2. Write Status |= ACKNOWLEDGE (1)
3. Write Status |= DRIVER (2)
4. Read DeviceFeatures; negotiate subset; write DriverFeatures
5. Write GuestPageSize = 4096
6. Configure virtqueues 0 (RX) and 1 (TX):
   - Write QueueSel = queue_index
   - Read QueueNumMax; write QueueNum = min(QueueNumMax, 256)
   - Allocate descriptor ring, available ring, used ring
   - Write QueuePFN = ring_physical_addr >> 12
7. Write Status |= FEATURES_OK (8)
8. Write Status |= DRIVER_OK (4)
```

**Descriptor ring (split virtqueue):**

```
Descriptor table: array of VirtqDesc
  { addr: u64, len: u32, flags: u16, next: u16 }

Available ring:
  { flags: u16, idx: u16, ring: [u16; N] }

Used ring:
  { flags: u16, idx: u16, ring: [VirtqUsedElem; N] }
    where VirtqUsedElem = { id: u32, len: u32 }
```

**TX path:** Place frame in descriptor → add to available ring → write `QueueNotify = 1` → poll used ring for completion.

**RX path:** Pre-fill descriptor ring with receive buffers → device fills them and adds to used ring → driver reads frames from used ring entries.

### 9.3 `net-smoltcp` Userspace Daemon

The `net-smoltcp` daemon is a userspace process (typically PID 2, spawned by `init`):

```
net-smoltcp startup:
  1. sys_net_recv / sys_net_send → raw Ethernet I/O
  2. smoltcp::iface::Interface created with EthernetInterface
  3. Loop:
       a. Poll smoltcp interface (processes timers, ARP, TCP state machines)
       b. sys_daemon_next_socket() → get newly registered socket IDs
       c. For each pending socket: process BIND/LISTEN/CONNECT opcodes
          from the socket's kernel channel
       d. For each TCP socket with received data: forward via kernel channel
          to the waiting application process
       e. sys_net_recv() → inject new Ethernet frames into smoltcp
       f. sys_net_send() → drain frames queued by smoltcp
```

The daemon uses smoltcp's TCP/IP state machine. Applications communicate with it exclusively through kernel channels — they never see raw Ethernet frames.

### 9.4 Socket Lifecycle

```
Application              Kernel                    net-smoltcp daemon
──────────               ──────                    ──────────────────
sys_socket()
  │                SocketRegistry.register(sid)
  │                Creates ChannelPair
  │                Returns user fd
  │
  │                                          sys_daemon_next_socket()
  │                                            ← pops sid from registry queue
  │
sys_bind(fd, addr)
  │                Sends BIND opcode msg
  │                  to socket's channel
  │                                          Reads BIND from channel
  │                                          Creates smoltcp socket
  │                                          Binds to addr
  │
sys_listen(fd, backlog)
  │                Sends LISTEN opcode
  │                                          Reads LISTEN
  │                                          Sets smoltcp socket to listen mode
  │
[incoming TCP connection]
  │                                          smoltcp accepts connection
  │                                          sys_daemon_create_conn(listen_sid)
  │                  Creates new ChannelPair for conn
  │                  Delivers server end to application (via wake)
  │
sys_accept(fd)
  │ ← blocks      Woken; returns new conn fd
  │
sys_sock_recv(conn_fd)
  │                                          Data arrives → sys_write to channel
  │ ← data                ← channel msg
```

---
[Back to Index](README.md)
