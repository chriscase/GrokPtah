#include "protocol.h"

#if !defined(__linux__) || !defined(__aarch64__)
#error "the v1 isolated visual guest is Linux arm64 only"
#endif

#define GPT_AF_VSOCK 40L
#define GPT_SOCK_STREAM 1L
#define GPT_SOCK_CLOEXEC 524288L
#define GPT_VMADDR_CID_HOST 2U

#define GPT_SYS_CLOSE 57L
#define GPT_SYS_LSEEK 62L
#define GPT_SYS_READ 63L
#define GPT_SYS_WRITE 64L
#define GPT_SYS_POLL 7L
#define GPT_SYS_OPENAT 56L
#define GPT_SYS_GETRANDOM 278L
#define GPT_SYS_NANOSLEEP 101L
#define GPT_SYS_REBOOT 142L
#define GPT_SYS_SOCKET 198L
#define GPT_SYS_CONNECT 203L
#define GPT_SYS_EXIT 93L

#define GPT_EINTR 4L
#define GPT_REBOOT_MAGIC1 0xfee1deadL
#define GPT_REBOOT_MAGIC2 0x28121969L
#define GPT_REBOOT_POWER_OFF 0x4321fedcL
#define GPT_AT_FDCWD (-100L)
#define GPT_O_RDONLY 0L
#define GPT_O_RDWR 2L
#define GPT_O_CLOEXEC 524288L
#define GPT_SEEK_SET 0L
#define GPT_POLLIN 1U
#define GPT_POLLOUT 4U
#define GPT_POLLERR 8U
#define GPT_POLLHUP 16U
#define GPT_POLLNVAL 32U
#define GPT_GUEST_IO_ATTEMPTS 300U
#define GPT_GUEST_IO_WAIT_MILLISECONDS 100L
#define GPT_GUEST_FRAME_WIDTH GPT_ISOLATED_VISUAL_MAX_DISPLAY_WIDTH
#define GPT_GUEST_FRAME_HEIGHT GPT_ISOLATED_VISUAL_MAX_DISPLAY_HEIGHT
#define GPT_GUEST_FRAME_BYTES (GPT_GUEST_FRAME_WIDTH * GPT_GUEST_FRAME_HEIGHT * 4U)

typedef struct {
    int descriptor;
    gpt_u16 events;
    gpt_u16 revents;
} gpt_pollfd;

typedef struct {
    unsigned short family;
    unsigned short reserved;
    unsigned int port;
    unsigned int cid;
    unsigned char zero[4];
} gpt_sockaddr_vm;

typedef struct {
    long seconds;
    long nanoseconds;
} gpt_timespec;

_Static_assert(sizeof(gpt_sockaddr_vm) == 16U, "unexpected sockaddr_vm layout");

static long gpt_syscall6(
    long number,
    long argument0,
    long argument1,
    long argument2,
    long argument3,
    long argument4,
    long argument5) {
    register long x0 __asm__("x0") = argument0;
    register long x1 __asm__("x1") = argument1;
    register long x2 __asm__("x2") = argument2;
    register long x3 __asm__("x3") = argument3;
    register long x4 __asm__("x4") = argument4;
    register long x5 __asm__("x5") = argument5;
    register long x8 __asm__("x8") = number;
    __asm__ volatile(
        "svc #0"
        : "+r"(x0)
        : "r"(x1), "r"(x2), "r"(x3), "r"(x4), "r"(x5), "r"(x8)
        : "memory");
    return x0;
}

static long gpt_syscall3(long number, long argument0, long argument1, long argument2) {
    return gpt_syscall6(number, argument0, argument1, argument2, 0, 0, 0);
}

static long gpt_read(int descriptor, void *bytes, gpt_size length) {
    return gpt_syscall3(GPT_SYS_READ, descriptor, (long)bytes, (long)length);
}

static long gpt_write(int descriptor, const void *bytes, gpt_size length) {
    return gpt_syscall3(GPT_SYS_WRITE, descriptor, (long)bytes, (long)length);
}

static int gpt_wait_for_io(int descriptor, gpt_u16 events) {
    unsigned int attempt;
    for (attempt = 0; attempt < GPT_GUEST_IO_ATTEMPTS; ++attempt) {
        gpt_pollfd descriptor_state = {
            .descriptor = descriptor,
            .events = events,
            .revents = 0,
        };
        long polled = gpt_syscall3(
            GPT_SYS_POLL,
            (long)&descriptor_state,
            1,
            GPT_GUEST_IO_WAIT_MILLISECONDS);
        if (polled == -GPT_EINTR) {
            continue;
        }
        if (polled <= 0 ||
            (descriptor_state.revents & (GPT_POLLERR | GPT_POLLHUP | GPT_POLLNVAL)) != 0 ||
            (descriptor_state.revents & events) == 0) {
            continue;
        }
        return 1;
    }
    return 0;
}

static int gpt_read_exact(int descriptor, gpt_u8 *bytes, gpt_size length) {
    gpt_size offset = 0;
    while (offset < length) {
        if (!gpt_wait_for_io(descriptor, GPT_POLLIN)) {
            return 0;
        }
        long count = gpt_read(descriptor, bytes + offset, length - offset);
        if (count == -GPT_EINTR) {
            continue;
        }
        if (count <= 0) {
            return 0;
        }
        offset += (gpt_size)count;
    }
    return 1;
}

static int gpt_write_exact(int descriptor, const gpt_u8 *bytes, gpt_size length) {
    gpt_size offset = 0;
    while (offset < length) {
        if (!gpt_wait_for_io(descriptor, GPT_POLLOUT)) {
            return 0;
        }
        long count = gpt_write(descriptor, bytes + offset, length - offset);
        if (count == -GPT_EINTR) {
            continue;
        }
        if (count <= 0) {
            return 0;
        }
        offset += (gpt_size)count;
    }
    return 1;
}

static void gpt_sleep_retry(void) {
    gpt_timespec delay = {.seconds = 0, .nanoseconds = 100000000L};
    while (gpt_syscall3(GPT_SYS_NANOSLEEP, (long)&delay, 0, 0) == -GPT_EINTR) {
    }
}

static int gpt_open_framebuffer(void) {
    static const gpt_u8 path[] = "/dev/fb0";
    long descriptor = gpt_syscall6(
        GPT_SYS_OPENAT,
        GPT_AT_FDCWD,
        (long)path,
        GPT_O_RDWR | GPT_O_CLOEXEC,
        0,
        0,
        0);
    return descriptor < 0 ? -1 : (int)descriptor;
}

static int gpt_capture_frame(int framebuffer, gpt_u8 *bytes) {
    gpt_size offset = 0;
    if (gpt_syscall3(GPT_SYS_LSEEK, framebuffer, 0, GPT_SEEK_SET) < 0) {
        return 0;
    }
    while (offset < GPT_GUEST_FRAME_BYTES) {
        long count = gpt_read(framebuffer, bytes + offset, GPT_GUEST_FRAME_BYTES - offset);
        if (count == -GPT_EINTR) {
            continue;
        }
        if (count <= 0) {
            return 0;
        }
        offset += (gpt_size)count;
    }
    return 1;
}

static int gpt_fill_frame_nonce(gpt_u8 nonce[16]) {
    long count = gpt_syscall3(GPT_SYS_GETRANDOM, (long)nonce, 16, 0);
    if (count != 16) {
        return 0;
    }
    nonce[6] = (gpt_u8)((nonce[6] & 0x0fU) | 0x40U);
    nonce[8] = (gpt_u8)((nonce[8] & 0x3fU) | 0x80U);
    return 1;
}

static gpt_u8 gpt_frame_bytes[GPT_GUEST_FRAME_BYTES];
static gpt_u8 gpt_frame_packet[GPT_ISOLATED_VISUAL_FRAME_MAX_PACKET_BYTES];
static gpt_u8 gpt_frame_authentication[GPT_ISOLATED_VISUAL_FRAME_AUTH_MAX_BYTES];

static gpt_u32 gpt_fixture_cursor_x = GPT_GUEST_FRAME_WIDTH / 2U;
static gpt_u32 gpt_fixture_cursor_y = GPT_GUEST_FRAME_HEIGHT / 2U;
static gpt_u8 gpt_fixture_button = 0;
static gpt_u32 gpt_fixture_generation = 0;

static void gpt_fixture_pixel(gpt_u8 *bytes, gpt_u32 x, gpt_u32 y, gpt_u32 color) {
    gpt_size offset = ((gpt_size)y * GPT_GUEST_FRAME_WIDTH + x) * 4U;
    bytes[offset] = (gpt_u8)color;
    bytes[offset + 1U] = (gpt_u8)(color >> 8U);
    bytes[offset + 2U] = (gpt_u8)(color >> 16U);
    bytes[offset + 3U] = (gpt_u8)(color >> 24U);
}

static void gpt_fixture_rect(
    gpt_u8 *bytes,
    gpt_u32 left,
    gpt_u32 top,
    gpt_u32 right,
    gpt_u32 bottom,
    gpt_u32 color) {
    gpt_u32 x;
    gpt_u32 y;
    if (left >= right || top >= bottom || left >= GPT_GUEST_FRAME_WIDTH ||
        top >= GPT_GUEST_FRAME_HEIGHT) {
        return;
    }
    if (right > GPT_GUEST_FRAME_WIDTH) {
        right = GPT_GUEST_FRAME_WIDTH;
    }
    if (bottom > GPT_GUEST_FRAME_HEIGHT) {
        bottom = GPT_GUEST_FRAME_HEIGHT;
    }
    for (y = top; y < bottom; ++y) {
        for (x = left; x < right; ++x) {
            gpt_fixture_pixel(bytes, x, y, color);
        }
    }
}

static int gpt_render_fixture(int framebuffer) {
    gpt_u32 x;
    gpt_u32 y;
    gpt_u32 generation = gpt_fixture_generation & 0x3fU;
    for (y = 0; y < GPT_GUEST_FRAME_HEIGHT; ++y) {
        for (x = 0; x < GPT_GUEST_FRAME_WIDTH; ++x) {
            gpt_u32 red = 0x18U + ((x * 0x38U) / GPT_GUEST_FRAME_WIDTH);
            gpt_u32 green = 0x20U + ((y * 0x42U) / GPT_GUEST_FRAME_HEIGHT);
            gpt_u32 blue = 0x38U + generation;
            gpt_fixture_pixel(
                gpt_frame_bytes,
                x,
                y,
                0xff000000U | (red << 16U) | (green << 8U) | blue);
        }
    }
    gpt_fixture_rect(
        gpt_frame_bytes,
        48U,
        40U,
        GPT_GUEST_FRAME_WIDTH - 48U,
        104U,
        0xff24324aU);
    gpt_fixture_rect(gpt_frame_bytes, 80U, 150U, 600U, 640U, 0xff182235U);
    gpt_fixture_rect(gpt_frame_bytes, 640U, 150U, 1200U, 640U, 0xff101827U);
    gpt_fixture_rect(
        gpt_frame_bytes,
        96U,
        184U,
        520U,
        gpt_fixture_button != 0U ? 340U : 300U,
        gpt_fixture_button != 0U ? 0xff4e9f72U : 0xff35527dU);
    gpt_fixture_rect(gpt_frame_bytes, 96U, 360U, 520U, 396U, 0xff41536eU);
    gpt_fixture_rect(gpt_frame_bytes, 96U, 440U, 520U, 476U, 0xff41536eU);
    gpt_fixture_rect(gpt_frame_bytes, 680U, 196U, 1160U, 228U, 0xff31445fU);
    gpt_fixture_rect(gpt_frame_bytes, 680U, 252U, 1080U, 284U, 0xff31445fU);
    gpt_fixture_rect(gpt_frame_bytes, 680U, 308U, 1120U, 340U, 0xff31445fU);
    gpt_fixture_rect(
        gpt_frame_bytes,
        gpt_fixture_cursor_x > 5U ? gpt_fixture_cursor_x - 5U : 0U,
        gpt_fixture_cursor_y > 5U ? gpt_fixture_cursor_y - 5U : 0U,
        gpt_fixture_cursor_x + 6U,
        gpt_fixture_cursor_y + 6U,
        gpt_fixture_button != 0U ? 0xffffd166U : 0xfff5f7fbU);
    if (gpt_syscall3(GPT_SYS_LSEEK, framebuffer, 0, GPT_SEEK_SET) < 0) {
        return 0;
    }
    return gpt_write_exact(framebuffer, gpt_frame_bytes, GPT_GUEST_FRAME_BYTES);
}

static void gpt_apply_fixture_input(const gpt_u8 *packet) {
    gpt_u8 kind = packet[40U];
    gpt_u8 state = packet[41U];
    gpt_u32 x = gpt_load_be32(packet + 44U);
    gpt_u32 y = gpt_load_be32(packet + 48U);
    gpt_i32 delta_x = (gpt_i32)gpt_load_be32(packet + 52U);
    gpt_i32 delta_y = (gpt_i32)gpt_load_be32(packet + 56U);
    if (kind == 1U || kind == 2U) {
        gpt_fixture_cursor_x = x;
        gpt_fixture_cursor_y = y;
        if (kind == 2U) {
            gpt_fixture_button = state == 1U ? 1U : 0U;
        }
    } else if (kind == 3U) {
        gpt_i32 next_x = (gpt_i32)gpt_fixture_cursor_x + delta_x;
        gpt_i32 next_y = (gpt_i32)gpt_fixture_cursor_y + delta_y;
        gpt_fixture_cursor_x = next_x < 0 ? 0U : (gpt_u32)next_x;
        gpt_fixture_cursor_y = next_y < 0 ? 0U : (gpt_u32)next_y;
        if (gpt_fixture_cursor_x >= GPT_GUEST_FRAME_WIDTH) {
            gpt_fixture_cursor_x = GPT_GUEST_FRAME_WIDTH - 1U;
        }
        if (gpt_fixture_cursor_y >= GPT_GUEST_FRAME_HEIGHT) {
            gpt_fixture_cursor_y = GPT_GUEST_FRAME_HEIGHT - 1U;
        }
    }
    gpt_fixture_generation += 1U;
}

static int gpt_send_frame(
    int socket,
    int framebuffer,
    const gpt_u8 *run_id,
    gpt_u16 run_id_bytes,
    const gpt_u8 *surface_id,
    gpt_u16 surface_id_bytes,
    const gpt_u8 *incarnation,
    gpt_u16 incarnation_bytes,
    const gpt_u8 channel_secret[GPT_GUEST_BOOTSTRAP_CHALLENGE_BYTES],
    gpt_u64 frame_sequence) {
    gpt_u8 content_sha256[32];
    gpt_u8 request_nonce[16];
    gpt_u8 length_bytes[4];
    gpt_u32 chunk_count =
        (GPT_GUEST_FRAME_BYTES + GPT_ISOLATED_VISUAL_FRAME_CHUNK_BYTES - 1U) /
        GPT_ISOLATED_VISUAL_FRAME_CHUNK_BYTES;
    gpt_u32 chunk_index;
    if (!gpt_render_fixture(framebuffer) || !gpt_capture_frame(framebuffer, gpt_frame_bytes) ||
        !gpt_fill_frame_nonce(request_nonce)) {
        return 0;
    }
    gpt_sha256(gpt_frame_bytes, GPT_GUEST_FRAME_BYTES, content_sha256);
    for (chunk_index = 0; chunk_index < chunk_count; ++chunk_index) {
        gpt_u64 offset = (gpt_u64)chunk_index * GPT_ISOLATED_VISUAL_FRAME_CHUNK_BYTES;
        gpt_u32 remaining = GPT_GUEST_FRAME_BYTES - (gpt_u32)offset;
        gpt_u32 payload_bytes = remaining > GPT_ISOLATED_VISUAL_FRAME_CHUNK_BYTES
                                    ? GPT_ISOLATED_VISUAL_FRAME_CHUNK_BYTES
                                    : remaining;
        gpt_u32 packet_bytes = 0;
        if (!gpt_isolated_visual_frame_seal(
                channel_secret,
                run_id,
                run_id_bytes,
                surface_id,
                surface_id_bytes,
                incarnation,
                incarnation_bytes,
                frame_sequence,
                request_nonce,
                chunk_index,
                chunk_count,
                GPT_GUEST_FRAME_BYTES,
                offset,
                GPT_GUEST_FRAME_WIDTH,
                GPT_GUEST_FRAME_HEIGHT,
                content_sha256,
                gpt_frame_bytes + offset,
                payload_bytes,
                gpt_frame_packet,
                sizeof(gpt_frame_packet),
                &packet_bytes,
                gpt_frame_authentication,
                sizeof(gpt_frame_authentication))) {
            return 0;
        }
        gpt_store_be32(length_bytes, packet_bytes);
        if (!gpt_write_exact(socket, length_bytes, sizeof(length_bytes)) ||
            !gpt_write_exact(socket, gpt_frame_packet, packet_bytes)) {
            return 0;
        }
    }
    return 1;
}

static int gpt_connect_to_host(void) {
    long descriptor = gpt_syscall3(
        GPT_SYS_SOCKET,
        GPT_AF_VSOCK,
        GPT_SOCK_STREAM | GPT_SOCK_CLOEXEC,
        0);
    if (descriptor < 0) {
        return -1;
    }
    gpt_sockaddr_vm address = {
        .family = (unsigned short)GPT_AF_VSOCK,
        .reserved = 0,
        .port = GPT_GUEST_BOOTSTRAP_PORT,
        .cid = GPT_VMADDR_CID_HOST,
        .zero = {0, 0, 0, 0},
    };
    unsigned int attempt;
    for (attempt = 0; attempt < 300U; ++attempt) {
        long result = gpt_syscall3(
            GPT_SYS_CONNECT,
            descriptor,
            (long)&address,
            sizeof(address));
        if (result == 0) {
            return (int)descriptor;
        }
        gpt_sleep_retry();
    }
    gpt_syscall3(GPT_SYS_CLOSE, descriptor, 0, 0);
    return -1;
}

__attribute__((noreturn)) static void gpt_power_off(long failure_code) {
    (void)gpt_syscall6(
        GPT_SYS_REBOOT,
        GPT_REBOOT_MAGIC1,
        GPT_REBOOT_MAGIC2,
        GPT_REBOOT_POWER_OFF,
        0,
        0,
        0);
    gpt_syscall3(GPT_SYS_EXIT, failure_code, 0, 0);
    for (;;) {
        __asm__ volatile("wfe");
    }
}

__attribute__((noreturn)) void _start(void) {
    int socket = gpt_connect_to_host();
    if (socket < 0) {
        gpt_power_off(20);
    }

    gpt_u8 challenge[GPT_GUEST_BOOTSTRAP_CHALLENGE_BYTES];
    gpt_u8 frame[GPT_GUEST_BOOTSTRAP_FRAME_BYTES];
    if (!gpt_read_exact(socket, challenge, sizeof(challenge)) ||
        !gpt_guest_bootstrap_frame(challenge, GPT_GUEST_BOOTSTRAP_EVENT_READY, frame) ||
        !gpt_write_exact(socket, frame, sizeof(frame))) {
        gpt_power_off(21);
    }

    gpt_u8 command = 0;
    gpt_u8 bound = 0;
    gpt_u8 channel_secret[GPT_GUEST_BOOTSTRAP_CHALLENGE_BYTES] = {0};
    gpt_u8 binding_payload[4U * GPT_ISOLATED_VISUAL_BINDING_MAX_FIELD_BYTES] = {0};
    gpt_u16 binding_lengths[4] = {0, 0, 0, 0};
    gpt_u64 last_frame_sequence = 0;
    gpt_u64 last_input_sequence = 0;
    gpt_u32 held_keys_mask = 0;
    gpt_u8 held_button = 0;
    int framebuffer = -1;
    for (;;) {
        if (bound) {
            gpt_pollfd descriptor = {
                .descriptor = socket,
                .events = GPT_POLLIN,
                .revents = 0,
            };
            long polled;
            do {
                polled = gpt_syscall3(GPT_SYS_POLL, (long)&descriptor, 1, 100);
            } while (polled == -GPT_EINTR);
            if (polled < 0 || (descriptor.revents & (GPT_POLLERR | GPT_POLLHUP)) != 0) {
                gpt_power_off(34);
            }
            if (polled == 0) {
                if (last_frame_sequence == ~0ULL ||
                    !gpt_send_frame(
                        socket,
                        framebuffer,
                        binding_payload,
                        binding_lengths[0],
                        binding_payload + binding_lengths[0],
                        binding_lengths[1],
                        binding_payload + binding_lengths[0] + binding_lengths[1],
                        binding_lengths[2],
                        channel_secret,
                        last_frame_sequence + 1U)) {
                    gpt_power_off(35);
                }
                last_frame_sequence += 1U;
                continue;
            }
        }
        if (!gpt_read_exact(socket, &command, 1U)) {
            gpt_power_off(22);
        }
        if (command == GPT_GUEST_BOOTSTRAP_STOP) {
            if (bound && (held_keys_mask != 0U || held_button != 0U)) {
                gpt_power_off(32);
            }
            if (!gpt_guest_bootstrap_frame(
                    challenge,
                    GPT_GUEST_BOOTSTRAP_EVENT_SHUTDOWN_ACK,
                    frame) ||
                !gpt_write_exact(socket, frame, sizeof(frame))) {
                gpt_power_off(22);
            }
            break;
        }
        if (command == GPT_GUEST_BOOTSTRAP_INPUT) {
            gpt_u8 length_bytes[4];
            gpt_u32 packet_bytes;
            gpt_u8 packet[GPT_ISOLATED_VISUAL_INPUT_MAX_PACKET_BYTES];
            if (!bound || !gpt_read_exact(socket, length_bytes, sizeof(length_bytes))) {
                gpt_power_off(23);
            }
            packet_bytes = gpt_load_be32(length_bytes);
            if (packet_bytes < GPT_ISOLATED_VISUAL_INPUT_HEADER_BYTES +
                                    GPT_ISOLATED_VISUAL_INPUT_TAG_BYTES ||
                packet_bytes > GPT_ISOLATED_VISUAL_INPUT_MAX_PACKET_BYTES ||
                !gpt_read_exact(socket, packet, packet_bytes) ||
                !gpt_isolated_visual_input_valid(
                    channel_secret,
                    binding_payload,
                    binding_lengths[0],
                    binding_payload + binding_lengths[0],
                    binding_lengths[1],
                    binding_payload + binding_lengths[0] + binding_lengths[1],
                    binding_lengths[2],
                    packet,
                    packet_bytes,
                    last_frame_sequence,
                    last_input_sequence,
                    GPT_ISOLATED_VISUAL_MAX_DISPLAY_WIDTH,
                    GPT_ISOLATED_VISUAL_MAX_DISPLAY_HEIGHT) ||
                !gpt_isolated_visual_input_apply_state(
                    packet,
                    &held_keys_mask,
                    &held_button)) {
                gpt_power_off(24);
            }
            gpt_apply_fixture_input(packet);
            last_input_sequence += 1U;
            continue;
        }
        if (command != GPT_GUEST_BOOTSTRAP_BIND) {
            gpt_power_off(25);
        }

        gpt_u8 binding_header[GPT_ISOLATED_VISUAL_BINDING_HEADER_BYTES];
        gpt_u8 candidate_payload[4U * GPT_ISOLATED_VISUAL_BINDING_MAX_FIELD_BYTES];
        gpt_u8 candidate_channel_secret[GPT_GUEST_BOOTSTRAP_CHALLENGE_BYTES];
        if (bound) {
            gpt_power_off(26);
        }
        if (!gpt_read_exact(socket, binding_header, sizeof(binding_header))) {
            gpt_power_off(27);
        }
        gpt_u16 lengths[4] = {
            gpt_load_be16(binding_header + 8U),
            gpt_load_be16(binding_header + 10U),
            gpt_load_be16(binding_header + 12U),
            gpt_load_be16(binding_header + 14U),
        };
        gpt_size binding_payload_bytes = 0;
        gpt_u32 index;
        for (index = 0; index < 4U; ++index) {
            if (lengths[index] == 0 ||
                lengths[index] > GPT_ISOLATED_VISUAL_BINDING_MAX_FIELD_BYTES) {
                gpt_power_off(28);
            }
            binding_payload_bytes += lengths[index];
        }
        if (binding_payload_bytes > 4U * GPT_ISOLATED_VISUAL_BINDING_MAX_FIELD_BYTES) {
            gpt_power_off(29);
        }
        if (!gpt_read_exact(socket, candidate_payload, binding_payload_bytes) ||
            !gpt_isolated_visual_binding_valid(
                challenge,
                binding_header,
                candidate_payload,
                binding_payload_bytes)) {
            gpt_power_off(30);
        }
        gpt_isolated_visual_channel_secret(
            challenge,
            binding_header + 16U,
            candidate_channel_secret);
        if (!gpt_guest_bootstrap_frame(
                challenge,
                GPT_GUEST_BOOTSTRAP_EVENT_BINDING_ACK,
                frame) ||
            !gpt_write_exact(socket, frame, sizeof(frame))) {
            gpt_power_off(31);
        }
        for (index = 0; index < 4U; ++index) {
            binding_lengths[index] = lengths[index];
        }
        for (index = 0; index < binding_payload_bytes; ++index) {
            binding_payload[index] = candidate_payload[index];
        }
        for (index = 0; index < GPT_GUEST_BOOTSTRAP_CHALLENGE_BYTES; ++index) {
            channel_secret[index] = candidate_channel_secret[index];
        }
        bound = 1;
        last_frame_sequence = 0;
        last_input_sequence = 0;
        held_keys_mask = 0;
        held_button = 0;
        framebuffer = gpt_open_framebuffer();
        if (framebuffer < 0) {
            gpt_power_off(33);
        }
    }
    gpt_syscall3(GPT_SYS_CLOSE, socket, 0, 0);
    gpt_power_off(0);
}
