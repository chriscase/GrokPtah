#include <stdio.h>
#include <string.h>

#include "protocol.h"

static int require_bytes(const gpt_u8 *observed, const gpt_u8 *expected, gpt_size length) {
    return gpt_constant_time_equal(observed, expected, length);
}

int main(void) {
    gpt_u8 helper_event[GPT_ISOLATED_HELPER_EVENT_BYTES] = {0};
    gpt_store_be32(helper_event, GPT_ISOLATED_HELPER_EVENT_MAGIC);
    gpt_store_be16(helper_event + 4U, GPT_ISOLATED_HELPER_EVENT_VERSION);
    gpt_store_be16(helper_event + 6U, GPT_ISOLATED_HELPER_EVENT_PREPARED);
    if (helper_event[0] != 'G' || helper_event[1] != 'P' || helper_event[2] != 'T' ||
        helper_event[3] != 'I' || sizeof(gpt_isolated_helper_event) !=
        GPT_ISOLATED_HELPER_EVENT_BYTES) {
        fputs("helper event ABI self-test failed\n", stderr);
        return 1;
    }
    if (sizeof(gpt_isolated_visual_frame_header) != GPT_ISOLATED_VISUAL_FRAME_HEADER_BYTES ||
        GPT_ISOLATED_VISUAL_FRAME_TAG_BYTES != 32U ||
        GPT_ISOLATED_VISUAL_FRAME_CHUNK_BYTES != 65536U) {
        fputs("isolated visual frame ABI self-test failed\n", stderr);
        return 1;
    }
    if (sizeof(gpt_isolated_visual_input_header) != GPT_ISOLATED_VISUAL_INPUT_HEADER_BYTES ||
        GPT_ISOLATED_VISUAL_INPUT_TAG_BYTES != 32U ||
        GPT_ISOLATED_VISUAL_INPUT_MAX_TEXT_BYTES != 4096U) {
        fputs("isolated visual input ABI self-test failed\n", stderr);
        return 1;
    }

    static const gpt_u8 expected_hmac[32] = {
        0xb0, 0x34, 0x4c, 0x61, 0xd8, 0xdb, 0x38, 0x53,
        0x5c, 0xa8, 0xaf, 0xce, 0xaf, 0x0b, 0xf1, 0x2b,
        0x88, 0x1d, 0xc2, 0x00, 0xc9, 0x83, 0x3d, 0xa7,
        0x26, 0xe9, 0x37, 0x6c, 0x2e, 0x32, 0xcf, 0xf7,
    };
    gpt_u8 key[20];
    memset(key, 0x0b, sizeof(key));
    gpt_u8 digest[32];
    gpt_hmac_sha256(
        key,
        sizeof(key),
        (const gpt_u8 *)"Hi There",
        sizeof("Hi There") - 1U,
        digest);
    if (!require_bytes(digest, expected_hmac, sizeof(digest))) {
        fputs("HMAC-SHA256 self-test failed\n", stderr);
        return 1;
    }

    gpt_u8 challenge[GPT_GUEST_BOOTSTRAP_CHALLENGE_BYTES];
    for (gpt_u32 index = 0; index < sizeof(challenge); ++index) {
        challenge[index] = (gpt_u8)index;
    }
    gpt_u8 ready[GPT_GUEST_BOOTSTRAP_FRAME_BYTES];
    gpt_u8 stopped[GPT_GUEST_BOOTSTRAP_FRAME_BYTES];
    if (!gpt_guest_bootstrap_frame(challenge, GPT_GUEST_BOOTSTRAP_EVENT_READY, ready) ||
        !gpt_guest_bootstrap_frame(
            challenge,
            GPT_GUEST_BOOTSTRAP_EVENT_SHUTDOWN_ACK,
            stopped) ||
        !gpt_guest_bootstrap_frame_valid(
            challenge,
            GPT_GUEST_BOOTSTRAP_EVENT_READY,
            ready) ||
        !gpt_guest_bootstrap_frame_valid(
            challenge,
            GPT_GUEST_BOOTSTRAP_EVENT_SHUTDOWN_ACK,
            stopped) ||
        require_bytes(ready, stopped, sizeof(ready))) {
        fputs("bootstrap frame self-test failed\n", stderr);
        return 1;
    }
    ready[20] ^= 1U;
    if (gpt_guest_bootstrap_frame_valid(
            challenge,
            GPT_GUEST_BOOTSTRAP_EVENT_READY,
            ready)) {
        fputs("tampered bootstrap frame was accepted\n", stderr);
        return 1;
    }
    ready[20] ^= 1U;
    if (gpt_guest_bootstrap_frame_valid(
            challenge,
            GPT_GUEST_BOOTSTRAP_EVENT_SHUTDOWN_ACK,
            ready)) {
        fputs("ready frame accepted as shutdown acknowledgement\n", stderr);
        return 1;
    }
    gpt_u8 wrong_challenge[GPT_GUEST_BOOTSTRAP_CHALLENGE_BYTES];
    memcpy(wrong_challenge, challenge, sizeof(wrong_challenge));
    wrong_challenge[0] ^= 1U;
    if (gpt_guest_bootstrap_frame_valid(
            wrong_challenge,
            GPT_GUEST_BOOTSTRAP_EVENT_READY,
            ready)) {
        fputs("bootstrap frame accepted with wrong challenge\n", stderr);
        return 1;
    }
    puts("isolated guest bootstrap protocol self-test: ok");
    return 0;
}
