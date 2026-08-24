#ifndef GROKPTAH_ISOLATED_VISUAL_GUEST_PROTOCOL_H
#define GROKPTAH_ISOLATED_VISUAL_GUEST_PROTOCOL_H

/* Freestanding bootstrap protocol shared by the measured guest and helper. */
typedef unsigned char gpt_u8;
typedef unsigned short gpt_u16;
typedef unsigned int gpt_u32;
typedef unsigned long long gpt_u64;
typedef unsigned long gpt_size;

#if defined(__GNUC__)
#define GPT_GUEST_UNUSED __attribute__((unused))
#else
#define GPT_GUEST_UNUSED
#endif

#define GPT_GUEST_BOOTSTRAP_CHALLENGE_BYTES 32U
#define GPT_GUEST_BOOTSTRAP_TAG_BYTES 32U
#define GPT_GUEST_BOOTSTRAP_FRAME_BYTES 44U
#define GPT_GUEST_BOOTSTRAP_MAGIC 0x47505447U
#define GPT_GUEST_BOOTSTRAP_VERSION 1U
#define GPT_GUEST_BOOTSTRAP_EVENT_READY 1U
#define GPT_GUEST_BOOTSTRAP_EVENT_SHUTDOWN_ACK 2U
#define GPT_GUEST_BOOTSTRAP_STOP 2U
#define GPT_GUEST_BOOTSTRAP_PORT 17001U

/* Fixed host-supervisor ABI emitted by the signed helper. */
#define GPT_ISOLATED_HELPER_EVENT_MAGIC 0x47505449U
#define GPT_ISOLATED_HELPER_EVENT_VERSION 1U
#define GPT_ISOLATED_HELPER_EVENT_BYTES 16U
#define GPT_ISOLATED_HELPER_EVENT_PREPARED 1U
#define GPT_ISOLATED_HELPER_EVENT_RUNNING 2U
#define GPT_ISOLATED_HELPER_EVENT_STOPPED 3U
#define GPT_ISOLATED_HELPER_EVENT_FAILURE 4U
#define GPT_ISOLATED_HELPER_CONTROL_START 1U
#define GPT_ISOLATED_HELPER_CONTROL_STOP 2U

/* Authenticated guest-to-host frame chunks. Input is intentionally absent. */
#define GPT_ISOLATED_VISUAL_FRAME_MAGIC 0x47505446U
#define GPT_ISOLATED_VISUAL_FRAME_VERSION 1U
#define GPT_ISOLATED_VISUAL_FRAME_HEADER_BYTES 100U
#define GPT_ISOLATED_VISUAL_FRAME_TAG_BYTES 32U
#define GPT_ISOLATED_VISUAL_FRAME_CHUNK_BYTES 65536U

/* Host-to-guest input packets. They never target the host input domain. */
#define GPT_ISOLATED_VISUAL_INPUT_MAGIC 0x47505441U
#define GPT_ISOLATED_VISUAL_INPUT_VERSION 1U
#define GPT_ISOLATED_VISUAL_INPUT_HEADER_BYTES 64U
#define GPT_ISOLATED_VISUAL_INPUT_TAG_BYTES 32U
#define GPT_ISOLATED_VISUAL_INPUT_MAX_TEXT_BYTES 4096U

typedef struct __attribute__((packed)) {
    gpt_u32 magic;
    gpt_u16 version;
    gpt_u16 code;
    gpt_u32 detail;
    gpt_u32 reserved;
} gpt_isolated_helper_event;

_Static_assert(
    sizeof(gpt_isolated_helper_event) == GPT_ISOLATED_HELPER_EVENT_BYTES,
    "unexpected isolated helper event layout");

typedef struct __attribute__((packed)) {
    gpt_u32 magic;
    gpt_u16 version;
    gpt_u16 protocol_version;
    gpt_u64 frame_sequence;
    gpt_u8 request_nonce[16];
    gpt_u32 chunk_index;
    gpt_u32 chunk_count;
    gpt_u64 total_bytes;
    gpt_u64 offset;
    gpt_u32 width;
    gpt_u32 height;
    gpt_u8 content_sha256[32];
    gpt_u32 chunk_bytes;
} gpt_isolated_visual_frame_header;

_Static_assert(
    sizeof(gpt_isolated_visual_frame_header) == GPT_ISOLATED_VISUAL_FRAME_HEADER_BYTES,
    "unexpected isolated visual frame header layout");

typedef struct __attribute__((packed)) {
    gpt_u32 magic;
    gpt_u16 version;
    gpt_u16 protocol_version;
    gpt_u64 frame_sequence;
    gpt_u64 input_sequence;
    gpt_u8 request_nonce[16];
    gpt_u8 kind;
    gpt_u8 state;
    gpt_u16 code;
    gpt_u32 x;
    gpt_u32 y;
    gpt_u32 delta_x;
    gpt_u32 delta_y;
    gpt_u32 text_bytes;
} gpt_isolated_visual_input_header;

_Static_assert(
    sizeof(gpt_isolated_visual_input_header) == GPT_ISOLATED_VISUAL_INPUT_HEADER_BYTES,
    "unexpected isolated visual input header layout");

typedef struct {
    gpt_u32 state[8];
    gpt_u64 bit_count;
    gpt_u8 block[64];
    gpt_u32 used;
} gpt_sha256_context;

static gpt_u32 gpt_rotr32(gpt_u32 value, gpt_u32 count) {
    return (value >> count) | (value << (32U - count));
}

static gpt_u32 gpt_load_be32(const gpt_u8 *bytes) {
    return ((gpt_u32)bytes[0] << 24U) | ((gpt_u32)bytes[1] << 16U) |
           ((gpt_u32)bytes[2] << 8U) | (gpt_u32)bytes[3];
}

static void gpt_store_be16(gpt_u8 *bytes, gpt_u16 value) {
    bytes[0] = (gpt_u8)(value >> 8U);
    bytes[1] = (gpt_u8)value;
}

static void gpt_store_be32(gpt_u8 *bytes, gpt_u32 value) {
    bytes[0] = (gpt_u8)(value >> 24U);
    bytes[1] = (gpt_u8)(value >> 16U);
    bytes[2] = (gpt_u8)(value >> 8U);
    bytes[3] = (gpt_u8)value;
}

static void gpt_sha256_transform(gpt_sha256_context *context, const gpt_u8 *block) {
    static const gpt_u32 constants[64] = {
        0x428a2f98U, 0x71374491U, 0xb5c0fbcfU, 0xe9b5dba5U, 0x3956c25bU,
        0x59f111f1U, 0x923f82a4U, 0xab1c5ed5U, 0xd807aa98U, 0x12835b01U,
        0x243185beU, 0x550c7dc3U, 0x72be5d74U, 0x80deb1feU, 0x9bdc06a7U,
        0xc19bf174U, 0xe49b69c1U, 0xefbe4786U, 0x0fc19dc6U, 0x240ca1ccU,
        0x2de92c6fU, 0x4a7484aaU, 0x5cb0a9dcU, 0x76f988daU, 0x983e5152U,
        0xa831c66dU, 0xb00327c8U, 0xbf597fc7U, 0xc6e00bf3U, 0xd5a79147U,
        0x06ca6351U, 0x14292967U, 0x27b70a85U, 0x2e1b2138U, 0x4d2c6dfcU,
        0x53380d13U, 0x650a7354U, 0x766a0abbU, 0x81c2c92eU, 0x92722c85U,
        0xa2bfe8a1U, 0xa81a664bU, 0xc24b8b70U, 0xc76c51a3U, 0xd192e819U,
        0xd6990624U, 0xf40e3585U, 0x106aa070U, 0x19a4c116U, 0x1e376c08U,
        0x2748774cU, 0x34b0bcb5U, 0x391c0cb3U, 0x4ed8aa4aU,
        0x5b9cca4fU, 0x682e6ff3U, 0x748f82eeU, 0x78a5636fU, 0x84c87814U,
        0x8cc70208U, 0x90befffaU, 0xa4506cebU, 0xbef9a3f7U, 0xc67178f2U,
    };
    gpt_u32 words[64];
    gpt_u32 index;
    for (index = 0; index < 16U; ++index) {
        words[index] = gpt_load_be32(block + (index * 4U));
    }
    for (index = 16U; index < 64U; ++index) {
        gpt_u32 s0 = gpt_rotr32(words[index - 15U], 7U) ^
                     gpt_rotr32(words[index - 15U], 18U) ^
                     (words[index - 15U] >> 3U);
        gpt_u32 s1 = gpt_rotr32(words[index - 2U], 17U) ^
                     gpt_rotr32(words[index - 2U], 19U) ^
                     (words[index - 2U] >> 10U);
        words[index] = words[index - 16U] + s0 + words[index - 7U] + s1;
    }

    gpt_u32 a = context->state[0];
    gpt_u32 b = context->state[1];
    gpt_u32 c = context->state[2];
    gpt_u32 d = context->state[3];
    gpt_u32 e = context->state[4];
    gpt_u32 f = context->state[5];
    gpt_u32 g = context->state[6];
    gpt_u32 h = context->state[7];
    for (index = 0; index < 64U; ++index) {
        gpt_u32 sum1 = gpt_rotr32(e, 6U) ^ gpt_rotr32(e, 11U) ^ gpt_rotr32(e, 25U);
        gpt_u32 choice = (e & f) ^ ((~e) & g);
        gpt_u32 temporary1 = h + sum1 + choice + constants[index] + words[index];
        gpt_u32 sum0 = gpt_rotr32(a, 2U) ^ gpt_rotr32(a, 13U) ^ gpt_rotr32(a, 22U);
        gpt_u32 majority = (a & b) ^ (a & c) ^ (b & c);
        gpt_u32 temporary2 = sum0 + majority;
        h = g;
        g = f;
        f = e;
        e = d + temporary1;
        d = c;
        c = b;
        b = a;
        a = temporary1 + temporary2;
    }
    context->state[0] += a;
    context->state[1] += b;
    context->state[2] += c;
    context->state[3] += d;
    context->state[4] += e;
    context->state[5] += f;
    context->state[6] += g;
    context->state[7] += h;
}

static void gpt_sha256_init(gpt_sha256_context *context) {
    static const gpt_u32 initial[8] = {
        0x6a09e667U, 0xbb67ae85U, 0x3c6ef372U, 0xa54ff53aU,
        0x510e527fU, 0x9b05688cU, 0x1f83d9abU, 0x5be0cd19U,
    };
    gpt_u32 index;
    for (index = 0; index < 8U; ++index) {
        context->state[index] = initial[index];
    }
    context->bit_count = 0;
    context->used = 0;
}

static void gpt_sha256_update(
    gpt_sha256_context *context,
    const gpt_u8 *bytes,
    gpt_size length) {
    gpt_size index = 0;
    context->bit_count += (gpt_u64)length * 8U;
    while (index < length) {
        gpt_u32 available = 64U - context->used;
        gpt_size remaining = length - index;
        gpt_u32 count = remaining < (gpt_size)available ? (gpt_u32)remaining : available;
        gpt_u32 cursor;
        for (cursor = 0; cursor < count; ++cursor) {
            context->block[context->used + cursor] = bytes[index + cursor];
        }
        context->used += count;
        index += count;
        if (context->used == 64U) {
            gpt_sha256_transform(context, context->block);
            context->used = 0;
        }
    }
}

static void gpt_sha256_finish(gpt_sha256_context *context, gpt_u8 output[32]) {
    gpt_u32 index;
    context->block[context->used++] = 0x80U;
    if (context->used > 56U) {
        while (context->used < 64U) {
            context->block[context->used++] = 0;
        }
        gpt_sha256_transform(context, context->block);
        context->used = 0;
    }
    while (context->used < 56U) {
        context->block[context->used++] = 0;
    }
    for (index = 0; index < 8U; ++index) {
        context->block[56U + index] =
            (gpt_u8)(context->bit_count >> ((7U - index) * 8U));
    }
    gpt_sha256_transform(context, context->block);
    for (index = 0; index < 8U; ++index) {
        gpt_store_be32(output + (index * 4U), context->state[index]);
    }
}

static void gpt_sha256(const gpt_u8 *bytes, gpt_size length, gpt_u8 output[32]) {
    gpt_sha256_context context;
    gpt_sha256_init(&context);
    gpt_sha256_update(&context, bytes, length);
    gpt_sha256_finish(&context, output);
}

static void gpt_hmac_sha256(
    const gpt_u8 *key,
    gpt_size key_length,
    const gpt_u8 *message,
    gpt_size message_length,
    gpt_u8 output[32]) {
    gpt_u8 normalized[64];
    gpt_u8 inner_digest[32];
    gpt_u8 pad[64];
    gpt_u32 index;
    for (index = 0; index < 64U; ++index) {
        normalized[index] = 0;
    }
    if (key_length > 64U) {
        gpt_sha256(key, key_length, normalized);
    } else {
        for (index = 0; index < key_length; ++index) {
            normalized[index] = key[index];
        }
    }
    for (index = 0; index < 64U; ++index) {
        pad[index] = normalized[index] ^ 0x36U;
    }
    gpt_sha256_context inner;
    gpt_sha256_init(&inner);
    gpt_sha256_update(&inner, pad, 64U);
    gpt_sha256_update(&inner, message, message_length);
    gpt_sha256_finish(&inner, inner_digest);
    for (index = 0; index < 64U; ++index) {
        pad[index] = normalized[index] ^ 0x5cU;
    }
    gpt_sha256_context outer;
    gpt_sha256_init(&outer);
    gpt_sha256_update(&outer, pad, 64U);
    gpt_sha256_update(&outer, inner_digest, 32U);
    gpt_sha256_finish(&outer, output);
}

static GPT_GUEST_UNUSED int gpt_constant_time_equal(
    const gpt_u8 *left,
    const gpt_u8 *right,
    gpt_size length) {
    gpt_u8 difference = 0;
    gpt_size index;
    for (index = 0; index < length; ++index) {
        difference |= left[index] ^ right[index];
    }
    return difference == 0;
}

static int gpt_guest_bootstrap_tag(
    const gpt_u8 challenge[GPT_GUEST_BOOTSTRAP_CHALLENGE_BYTES],
    gpt_u16 event,
    gpt_u8 output[GPT_GUEST_BOOTSTRAP_TAG_BYTES]) {
    static const gpt_u8 ready[] = "grokptah-isolated-guest-ready-v1";
    static const gpt_u8 shutdown[] = "grokptah-isolated-guest-shutdown-v1";
    const gpt_u8 *message;
    gpt_size message_length;
    if (event == GPT_GUEST_BOOTSTRAP_EVENT_READY) {
        message = ready;
        message_length = sizeof(ready) - 1U;
    } else if (event == GPT_GUEST_BOOTSTRAP_EVENT_SHUTDOWN_ACK) {
        message = shutdown;
        message_length = sizeof(shutdown) - 1U;
    } else {
        return 0;
    }
    gpt_hmac_sha256(
        challenge,
        GPT_GUEST_BOOTSTRAP_CHALLENGE_BYTES,
        message,
        message_length,
        output);
    return 1;
}

static GPT_GUEST_UNUSED int gpt_guest_bootstrap_frame(
    const gpt_u8 challenge[GPT_GUEST_BOOTSTRAP_CHALLENGE_BYTES],
    gpt_u16 event,
    gpt_u8 output[GPT_GUEST_BOOTSTRAP_FRAME_BYTES]) {
    gpt_u32 index;
    gpt_store_be32(output, GPT_GUEST_BOOTSTRAP_MAGIC);
    gpt_store_be16(output + 4U, GPT_GUEST_BOOTSTRAP_VERSION);
    gpt_store_be16(output + 6U, event);
    for (index = 8U; index < 12U; ++index) {
        output[index] = 0;
    }
    return gpt_guest_bootstrap_tag(challenge, event, output + 12U);
}

static GPT_GUEST_UNUSED int gpt_guest_bootstrap_frame_valid(
    const gpt_u8 challenge[GPT_GUEST_BOOTSTRAP_CHALLENGE_BYTES],
    gpt_u16 expected_event,
    const gpt_u8 frame[GPT_GUEST_BOOTSTRAP_FRAME_BYTES]) {
    gpt_u8 expected_tag[GPT_GUEST_BOOTSTRAP_TAG_BYTES];
    if (gpt_load_be32(frame) != GPT_GUEST_BOOTSTRAP_MAGIC ||
        (((gpt_u16)frame[4] << 8U) | frame[5]) != GPT_GUEST_BOOTSTRAP_VERSION ||
        (((gpt_u16)frame[6] << 8U) | frame[7]) != expected_event || frame[8] != 0 ||
        frame[9] != 0 || frame[10] != 0 || frame[11] != 0 ||
        !gpt_guest_bootstrap_tag(challenge, expected_event, expected_tag)) {
        return 0;
    }
    return gpt_constant_time_equal(
        expected_tag,
        frame + 12U,
        GPT_GUEST_BOOTSTRAP_TAG_BYTES);
}

#endif
