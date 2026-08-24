#include "protocol.h"

#if !defined(__linux__) || !defined(__aarch64__)
#error "the v1 isolated visual guest is Linux arm64 only"
#endif

#define GPT_AF_VSOCK 40L
#define GPT_SOCK_STREAM 1L
#define GPT_SOCK_CLOEXEC 524288L
#define GPT_VMADDR_CID_HOST 2U

#define GPT_SYS_CLOSE 57L
#define GPT_SYS_READ 63L
#define GPT_SYS_WRITE 64L
#define GPT_SYS_NANOSLEEP 101L
#define GPT_SYS_REBOOT 142L
#define GPT_SYS_SOCKET 198L
#define GPT_SYS_CONNECT 203L
#define GPT_SYS_EXIT 93L

#define GPT_EINTR 4L
#define GPT_REBOOT_MAGIC1 0xfee1deadL
#define GPT_REBOOT_MAGIC2 0x28121969L
#define GPT_REBOOT_POWER_OFF 0x4321fedcL

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

static int gpt_read_exact(int descriptor, gpt_u8 *bytes, gpt_size length) {
    gpt_size offset = 0;
    while (offset < length) {
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
    for (;;) {
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
    }
    gpt_syscall3(GPT_SYS_CLOSE, socket, 0, 0);
    gpt_power_off(0);
}
