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
        GPT_ISOLATED_VISUAL_FRAME_CHUNK_BYTES != 65536U ||
        GPT_ISOLATED_VISUAL_FRAME_MAX_PACKET_BYTES != 65668U) {
        fputs("isolated visual frame ABI self-test failed\n", stderr);
        return 1;
    }
    if (sizeof(gpt_isolated_visual_input_header) != GPT_ISOLATED_VISUAL_INPUT_HEADER_BYTES ||
        GPT_ISOLATED_VISUAL_INPUT_TAG_BYTES != 32U ||
        GPT_ISOLATED_VISUAL_INPUT_MAX_TEXT_BYTES != 4096U) {
        fputs("isolated visual input ABI self-test failed\n", stderr);
        return 1;
    }
    if (sizeof(gpt_isolated_visual_binding_header) !=
            GPT_ISOLATED_VISUAL_BINDING_HEADER_BYTES ||
        GPT_ISOLATED_VISUAL_BINDING_DIGEST_BYTES != 32U ||
        GPT_ISOLATED_VISUAL_BINDING_TAG_BYTES != 32U) {
        fputs("isolated visual binding ABI self-test failed\n", stderr);
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
    gpt_u8 invalid_input[GPT_ISOLATED_VISUAL_INPUT_HEADER_BYTES +
                        GPT_ISOLATED_VISUAL_INPUT_TAG_BYTES] = {0};
    if (GPT_GUEST_BOOTSTRAP_INPUT != 4U ||
        GPT_ISOLATED_VISUAL_INPUT_MAX_PACKET_BYTES != 4192U ||
        gpt_isolated_visual_input_valid(
            challenge,
            (const gpt_u8 *)"run",
            3U,
            (const gpt_u8 *)"surface",
            7U,
            (const gpt_u8 *)"incarnation",
            11U,
            invalid_input,
            sizeof(invalid_input),
            1U,
            0U,
            1280U,
            800U)) {
        fputs("invalid isolated input packet was accepted\n", stderr);
        return 1;
    }
    static const gpt_u8 frame_nonce[16] = {
        0x40, 0x00, 0x00, 0x00, 0x00, 0x00, 0x40, 0x00,
        0x80, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01,
    };
    static const gpt_u8 frame_payload[] = {1, 2, 3, 4};
    static gpt_u8 frame_packet[GPT_ISOLATED_VISUAL_FRAME_MAX_PACKET_BYTES];
    static gpt_u8 frame_authentication[GPT_ISOLATED_VISUAL_FRAME_AUTH_MAX_BYTES];
    static const gpt_u8 frame_tag_expected[32] = {
        0xc7, 0x16, 0x11, 0xfa, 0x11, 0xd7, 0x6f, 0x0e,
        0x10, 0x09, 0x05, 0x4a, 0x18, 0x79, 0x10, 0x33,
        0x13, 0x10, 0xc1, 0x70, 0xa7, 0xc4, 0xbb, 0xf4,
        0xd7, 0xee, 0x93, 0x73, 0x52, 0x56, 0x08, 0x33,
    };
    gpt_u8 frame_digest[32] = {0};
    gpt_u32 frame_packet_bytes = 0;
    if (!gpt_isolated_visual_frame_seal(
            challenge,
            (const gpt_u8 *)"run",
            3U,
            (const gpt_u8 *)"surface",
            7U,
            (const gpt_u8 *)"incarnation",
            11U,
            1U,
            frame_nonce,
            0U,
            1U,
            sizeof(frame_payload),
            0U,
            1280U,
            800U,
            frame_digest,
            frame_payload,
            sizeof(frame_payload),
            frame_packet,
            sizeof(frame_packet),
            &frame_packet_bytes,
            frame_authentication,
            sizeof(frame_authentication)) ||
        frame_packet_bytes != GPT_ISOLATED_VISUAL_FRAME_HEADER_BYTES +
                                  sizeof(frame_payload) +
                                  GPT_ISOLATED_VISUAL_FRAME_TAG_BYTES ||
        gpt_load_be32(frame_packet) != GPT_ISOLATED_VISUAL_FRAME_MAGIC ||
        gpt_load_be32(frame_packet + 96U) != sizeof(frame_payload) ||
        !require_bytes(
            frame_packet + GPT_ISOLATED_VISUAL_FRAME_HEADER_BYTES + sizeof(frame_payload),
            frame_tag_expected,
            sizeof(frame_tag_expected))) {
        fputs("isolated frame seal self-test failed\n", stderr);
        return 1;
    }
    gpt_u8 state_packet[GPT_ISOLATED_VISUAL_INPUT_HEADER_BYTES +
                       GPT_ISOLATED_VISUAL_INPUT_TAG_BYTES] = {0};
    gpt_u32 held_keys_mask = 0;
    gpt_u8 held_button = 0;
    gpt_store_be16(state_packet + 42U, 1U);
    state_packet[40U] = 2U;
    state_packet[41U] = 1U;
    if (!gpt_isolated_visual_input_apply_state(
            state_packet,
            &held_keys_mask,
            &held_button) ||
        held_button != 1U ||
        gpt_isolated_visual_input_apply_state(
            state_packet,
            &held_keys_mask,
            &held_button)) {
        fputs("duplicate button down was accepted\n", stderr);
        return 1;
    }
    state_packet[41U] = 2U;
    state_packet[42U] = 0U;
    state_packet[43U] = 2U;
    if (gpt_isolated_visual_input_apply_state(
            state_packet,
            &held_keys_mask,
            &held_button) ||
        held_button != 1U) {
        fputs("mismatched button up was accepted or state changed\n", stderr);
        return 1;
    }
    state_packet[43U] = 1U;
    if (!gpt_isolated_visual_input_apply_state(
            state_packet,
            &held_keys_mask,
            &held_button) ||
        held_button != 0U) {
        fputs("matching button up did not clear held state\n", stderr);
        return 1;
    }
    state_packet[40U] = 4U;
    state_packet[41U] = 1U;
    state_packet[42U] = 0U;
    state_packet[43U] = 3U;
    if (!gpt_isolated_visual_input_apply_state(
            state_packet,
            &held_keys_mask,
            &held_button) ||
        (held_keys_mask & (1U << 2U)) == 0U ||
        gpt_isolated_visual_input_apply_state(
            state_packet,
            &held_keys_mask,
            &held_button)) {
        fputs("duplicate key down was accepted\n", stderr);
        return 1;
    }
    state_packet[41U] = 2U;
    if (!gpt_isolated_visual_input_apply_state(
            state_packet,
            &held_keys_mask,
            &held_button) ||
        held_keys_mask != 0U) {
        fputs("key up did not clear held state\n", stderr);
        return 1;
    }
    gpt_u8 ready[GPT_GUEST_BOOTSTRAP_FRAME_BYTES];
    gpt_u8 stopped[GPT_GUEST_BOOTSTRAP_FRAME_BYTES];
    gpt_u8 binding_ack[GPT_GUEST_BOOTSTRAP_FRAME_BYTES];
    if (!gpt_guest_bootstrap_frame(challenge, GPT_GUEST_BOOTSTRAP_EVENT_READY, ready) ||
        !gpt_guest_bootstrap_frame(
            challenge,
            GPT_GUEST_BOOTSTRAP_EVENT_SHUTDOWN_ACK,
            stopped) ||
        !gpt_guest_bootstrap_frame(
            challenge,
            GPT_GUEST_BOOTSTRAP_EVENT_BINDING_ACK,
            binding_ack) ||
        !gpt_guest_bootstrap_frame_valid(
            challenge,
            GPT_GUEST_BOOTSTRAP_EVENT_READY,
            ready) ||
        !gpt_guest_bootstrap_frame_valid(
            challenge,
            GPT_GUEST_BOOTSTRAP_EVENT_SHUTDOWN_ACK,
            stopped) ||
        !gpt_guest_bootstrap_frame_valid(
            challenge,
            GPT_GUEST_BOOTSTRAP_EVENT_BINDING_ACK,
            binding_ack) ||
        require_bytes(ready, stopped, sizeof(ready)) ||
        require_bytes(ready, binding_ack, sizeof(ready))) {
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

    static const gpt_u8 binding_digest_expected[32] = {
        0xbb, 0xfa, 0x00, 0x3d, 0x74, 0xa3, 0x72, 0xbc,
        0xf2, 0x33, 0xbe, 0xe0, 0x74, 0x5f, 0x75, 0xd5,
        0xa7, 0xf2, 0x55, 0x19, 0xac, 0xd9, 0xfb, 0x8e,
        0xdb, 0x14, 0x18, 0xfe, 0xec, 0xfa, 0x3d, 0x5c,
    };
    static const gpt_u8 run_id[] = "run-1";
    static const gpt_u8 surface_id[] = "surface-1";
    static const gpt_u8 incarnation[] = "incarnation-1";
    static const gpt_u8 input_domain[] = "domain-1";
    gpt_u8 binding_digest[GPT_ISOLATED_VISUAL_BINDING_DIGEST_BYTES];
    if (!gpt_isolated_visual_binding_digest(
            run_id,
            sizeof(run_id) - 1U,
            surface_id,
            sizeof(surface_id) - 1U,
            incarnation,
            sizeof(incarnation) - 1U,
            input_domain,
            sizeof(input_domain) - 1U,
            binding_digest) ||
        !require_bytes(binding_digest, binding_digest_expected, sizeof(binding_digest))) {
        fputs("isolated visual binding digest self-test failed\n", stderr);
        return 1;
    }
    gpt_u8 channel_secret[GPT_GUEST_BOOTSTRAP_CHALLENGE_BYTES];
    gpt_u8 confirmation[GPT_ISOLATED_VISUAL_BINDING_TAG_BYTES];
    gpt_isolated_visual_channel_secret(challenge, binding_digest, channel_secret);
    gpt_isolated_visual_binding_confirmation(channel_secret, binding_digest, confirmation);
    gpt_isolated_visual_binding_header binding = {0};
    gpt_store_be32((gpt_u8 *)&binding, GPT_ISOLATED_VISUAL_BINDING_MAGIC);
    gpt_store_be16((gpt_u8 *)&binding + 4U, GPT_ISOLATED_VISUAL_BINDING_VERSION);
    gpt_store_be16((gpt_u8 *)&binding + 6U, GPT_GUEST_BOOTSTRAP_VERSION);
    gpt_store_be16((gpt_u8 *)&binding + 8U, sizeof(run_id) - 1U);
    gpt_store_be16((gpt_u8 *)&binding + 10U, sizeof(surface_id) - 1U);
    gpt_store_be16((gpt_u8 *)&binding + 12U, sizeof(incarnation) - 1U);
    gpt_store_be16((gpt_u8 *)&binding + 14U, sizeof(input_domain) - 1U);
    memcpy(binding.binding_digest, binding_digest, sizeof(binding_digest));
    memcpy(binding.confirmation_tag, confirmation, sizeof(confirmation));
    gpt_u8 binding_payload[sizeof(run_id) + sizeof(surface_id) + sizeof(incarnation) +
                           sizeof(input_domain) - 4U];
    gpt_size binding_offset = 0;
    memcpy(binding_payload + binding_offset, run_id, sizeof(run_id) - 1U);
    binding_offset += sizeof(run_id) - 1U;
    memcpy(binding_payload + binding_offset, surface_id, sizeof(surface_id) - 1U);
    binding_offset += sizeof(surface_id) - 1U;
    memcpy(binding_payload + binding_offset, incarnation, sizeof(incarnation) - 1U);
    binding_offset += sizeof(incarnation) - 1U;
    memcpy(binding_payload + binding_offset, input_domain, sizeof(input_domain) - 1U);
    binding_offset += sizeof(input_domain) - 1U;
    if (!gpt_isolated_visual_binding_valid(
            challenge,
            (const gpt_u8 *)&binding,
            binding_payload,
            binding_offset)) {
        fputs("isolated visual binding packet was rejected\n", stderr);
        return 1;
    }
    binding_payload[0] ^= 1U;
    if (gpt_isolated_visual_binding_valid(
            challenge,
            (const gpt_u8 *)&binding,
            binding_payload,
            binding_offset)) {
        fputs("tampered isolated visual binding was accepted\n", stderr);
        return 1;
    }
    puts("isolated guest bootstrap protocol self-test: ok");
    return 0;
}
