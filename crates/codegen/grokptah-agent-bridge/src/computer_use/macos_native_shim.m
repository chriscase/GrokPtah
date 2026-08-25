#import <AppKit/AppKit.h>
#import <ApplicationServices/ApplicationServices.h>
#import <CoreGraphics/CoreGraphics.h>
#import <Foundation/Foundation.h>
#import <ScreenCaptureKit/ScreenCaptureKit.h>
#import <Security/Security.h>

#include <stdbool.h>
#include <dlfcn.h>
#include <fcntl.h>
#include <limits.h>
#include <stdio.h>
#include <stdatomic.h>
#include <stdint.h>
#include <math.h>
#include <stdlib.h>
#include <spawn.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

typedef struct {
    int32_t status;
    uint8_t *json;
    size_t json_len;
    uint8_t *png;
    size_t png_len;
    uint8_t *error;
    size_t error_len;
} GPTMacNativeResult;

typedef struct {
    bool operating_system_supported;
    bool framework_available;
} GPTMacVirtualizationProbe;

typedef struct {
    int32_t status;
    int32_t helper_fd;
    int32_t guest_image_fd;
    int32_t configuration_fd;
    uint8_t *requirement_data;
    size_t requirement_data_len;
    uint8_t *error;
    size_t error_len;
} GPTMacIsolatedArtifactsResult;

typedef struct {
    int32_t status;
    int32_t pid;
    int32_t control_fd;
    int32_t event_fd;
    int32_t input_fd;
    int32_t frame_fd;
    int32_t challenge_fd;
    uint8_t *error;
    size_t error_len;
} GPTMacIsolatedRuntimeSpawnResult;

enum {
    // Keep the inherited descriptor contract named at the native boundary;
    // the helper and source verifier use the same fixed private channel.
    GPT_CHALLENGE_FD = 9,
};

enum {
    GPT_MAC_OK = 0,
    GPT_MAC_UNSUPPORTED = 1,
    GPT_MAC_PERMISSION_REQUIRED = 2,
    GPT_MAC_TARGET_CLOSED = 3,
    GPT_MAC_TARGET_CHANGED = 4,
    GPT_MAC_SENSITIVE = 5,
    GPT_MAC_LIMIT_REACHED = 6,
    GPT_MAC_BACKEND_FAILURE = 7,
    GPT_MAC_INVALID_REQUEST = 8,
    GPT_MAC_FORBIDDEN_ACTION = 9,
    GPT_MAC_INTERRUPTED = 10,
    GPT_MAC_UNCERTAIN_OUTCOME = 11,
    GPT_MAC_BACKEND_UNAVAILABLE = 12,
    GPT_MAC_UNAUTHORIZED = 13,
};

typedef struct {
    atomic_bool signalled;
} GPTMacActionCancellation;

void *gpt_macos_cancellation_create(void) {
    GPTMacActionCancellation *cancellation = malloc(sizeof(GPTMacActionCancellation));
    if (cancellation != NULL) {
        atomic_init(&cancellation->signalled, false);
    }
    return cancellation;
}

void gpt_macos_cancellation_signal(void *context) {
    if (context != NULL) {
        GPTMacActionCancellation *cancellation = context;
        atomic_store_explicit(&cancellation->signalled, true, memory_order_seq_cst);
    }
}

bool gpt_macos_cancellation_is_signalled(const void *context) {
    if (context == NULL) {
        return true;
    }
    const GPTMacActionCancellation *cancellation = context;
    return atomic_load_explicit(&cancellation->signalled, memory_order_seq_cst);
}

void gpt_macos_cancellation_free(void *context) {
    free(context);
}

typedef struct {
    BOOL valid;
    pid_t frontmost_process_id;
    uint32_t active_window_id;
    CGPoint pointer_location;
} GPTMacUserInteractionState;

static GPTMacUserInteractionState GPTCaptureUserInteractionState(void) {
    GPTMacUserInteractionState state = {0};
    NSRunningApplication *frontmost = NSWorkspace.sharedWorkspace.frontmostApplication;
    if (frontmost == nil || frontmost.processIdentifier <= 0) {
        return state;
    }
    // Read-only AppKit snapshot of the physical pointer. Do not sample or
    // synthesize input through the Quartz event-injection family.
    state.pointer_location = [NSEvent mouseLocation];
    state.frontmost_process_id = frontmost.processIdentifier;

    CFArrayRef windowInfo = CGWindowListCopyWindowInfo(
        kCGWindowListOptionOnScreenOnly | kCGWindowListExcludeDesktopElements,
        kCGNullWindowID);
    if (windowInfo == NULL) {
        return state;
    }
    for (NSDictionary *entry in (__bridge NSArray *)windowInfo) {
        NSNumber *owner = entry[(id)kCGWindowOwnerPID];
        NSNumber *layer = entry[(id)kCGWindowLayer];
        NSNumber *window = entry[(id)kCGWindowNumber];
        if ([owner isKindOfClass:[NSNumber class]] &&
            owner.intValue == state.frontmost_process_id &&
            [layer isKindOfClass:[NSNumber class]] && layer.intValue == 0 &&
            [window isKindOfClass:[NSNumber class]] && window.unsignedIntValue != 0) {
            state.active_window_id = window.unsignedIntValue;
            break;
        }
    }
    CFRelease(windowInfo);
    state.valid = state.active_window_id != 0;
    return state;
}

static BOOL GPTUserInteractionStateEqual(
    GPTMacUserInteractionState before,
    GPTMacUserInteractionState after) {
    return before.valid && after.valid &&
           before.frontmost_process_id == after.frontmost_process_id &&
           before.active_window_id == after.active_window_id &&
           CGPointEqualToPoint(before.pointer_location, after.pointer_location);
}

static const NSUInteger GPT_MAX_AX_DEPTH = 32;
static const NSUInteger GPT_MAX_NATIVE_SCREENSHOT_DIMENSION = 4096;
static const size_t GPT_MAX_RAW_SCREENSHOT_BYTES = 64 * 1024 * 1024;

static GPTMacNativeResult GPTEmptyResult(int32_t status) {
    GPTMacNativeResult result = {0};
    result.status = status;
    return result;
}

static uint8_t *GPTCopyBytes(NSData *data, size_t *length) {
    if (data == nil || data.length == 0) {
        *length = 0;
        return NULL;
    }
    uint8_t *copy = malloc(data.length);
    if (copy == NULL) {
        *length = 0;
        return NULL;
    }
    memcpy(copy, data.bytes, data.length);
    *length = data.length;
    return copy;
}

static GPTMacNativeResult GPTErrorResult(int32_t status, NSString *message) {
    GPTMacNativeResult result = GPTEmptyResult(status);
    NSString *bounded = message.length > 512 ? [message substringToIndex:512] : message;
    NSData *data = [bounded dataUsingEncoding:NSUTF8StringEncoding];
    result.error = GPTCopyBytes(data, &result.error_len);
    return result;
}

static GPTMacNativeResult GPTJSONResult(id object, NSData *png) {
    NSError *error = nil;
    NSData *json = [NSJSONSerialization dataWithJSONObject:object options:0 error:&error];
    if (json == nil) {
        return GPTErrorResult(GPT_MAC_BACKEND_FAILURE, @"macOS observation JSON encoding failed");
    }
    GPTMacNativeResult result = GPTEmptyResult(GPT_MAC_OK);
    result.json = GPTCopyBytes(json, &result.json_len);
    result.png = GPTCopyBytes(png, &result.png_len);
    if (result.json == NULL || (png != nil && result.png == NULL)) {
        free(result.json);
        free(result.png);
        return GPTErrorResult(GPT_MAC_BACKEND_FAILURE, @"macOS observation allocation failed");
    }
    return result;
}

void gpt_macos_result_free(GPTMacNativeResult *result) {
    if (result == NULL) {
        return;
    }
    free(result->json);
    free(result->png);
    free(result->error);
    memset(result, 0, sizeof(*result));
}

static BOOL GPTLoadScreenCaptureKit(void) {
    static BOOL loaded = NO;
    static dispatch_once_t once;
    dispatch_once(&once, ^{
        void *handle = dlopen(
            "/System/Library/Frameworks/ScreenCaptureKit.framework/ScreenCaptureKit",
            RTLD_LAZY | RTLD_LOCAL);
        loaded = handle != NULL && NSClassFromString(@"SCShareableContent") != nil &&
                 NSClassFromString(@"SCScreenshotManager") != nil &&
                 NSClassFromString(@"SCContentFilter") != nil &&
                 NSClassFromString(@"SCStreamConfiguration") != nil;
    });
    return loaded;
}

bool gpt_macos_observation_supported(void) {
    if (@available(macOS 14.0, *)) {
        return GPTLoadScreenCaptureKit();
    }
    return false;
}

GPTMacVirtualizationProbe gpt_macos_virtualization_probe(void) {
    GPTMacVirtualizationProbe result = {0};
    if (@available(macOS 14.0, *)) {
        result.operating_system_supported = true;
    } else {
        return result;
    }

    static BOOL framework_available = NO;
    static dispatch_once_t once;
    dispatch_once(&once, ^{
        void *handle = dlopen(
            "/System/Library/Frameworks/Virtualization.framework/Virtualization",
            RTLD_LAZY | RTLD_LOCAL);
        framework_available = handle != NULL &&
                              NSClassFromString(@"VZVirtualMachine") != nil &&
                              NSClassFromString(@"VZVirtualMachineConfiguration") != nil &&
                              NSClassFromString(@"VZVirtioGraphicsDeviceConfiguration") != nil &&
                              NSClassFromString(@"VZVirtioSocketDeviceConfiguration") != nil &&
                              NSClassFromString(@"VZVirtualMachineView") != nil;
    });
    result.framework_available = framework_available;

    return result;
}

static const size_t GPT_MAX_REQUIREMENT_DATA_BYTES = 64 * 1024;
static NSString *const GPT_APP_BUNDLE_IDENTIFIER = @"com.chriscase.grokptah";
static NSString *const GPT_HELPER_SIGNING_IDENTIFIER =
    @"com.chriscase.grokptah.isolated-visual-helper";

static GPTMacIsolatedArtifactsResult GPTEmptyIsolatedArtifactsResult(int32_t status) {
    GPTMacIsolatedArtifactsResult result = {0};
    result.status = status;
    result.helper_fd = -1;
    result.guest_image_fd = -1;
    result.configuration_fd = -1;
    return result;
}

static GPTMacIsolatedArtifactsResult GPTIsolatedArtifactsError(
    int32_t status,
    NSString *message) {
    GPTMacIsolatedArtifactsResult result = GPTEmptyIsolatedArtifactsResult(status);
    NSString *bounded = message.length > 512 ? [message substringToIndex:512] : message;
    NSData *data = [bounded dataUsingEncoding:NSUTF8StringEncoding];
    result.error = GPTCopyBytes(data, &result.error_len);
    return result;
}

static void GPTCloseArtifactDescriptors(
    int helperFD,
    int guestImageFD,
    int configurationFD) {
    if (helperFD >= 0) {
        close(helperFD);
    }
    if (guestImageFD >= 0) {
        close(guestImageFD);
    }
    if (configurationFD >= 0) {
        close(configurationFD);
    }
}

static BOOL GPTAppendBundlePath(
    const char *bundleRoot,
    const char *relativePath,
    char output[PATH_MAX]) {
    int written = snprintf(output, PATH_MAX, "%s/%s", bundleRoot, relativePath);
    return written > 0 && written < PATH_MAX;
}

static int GPTOpenPackagedArtifact(const char *path) {
    return open(path, O_RDONLY | O_CLOEXEC | O_NOFOLLOW_ANY);
}

static BOOL GPTSameArtifactIdentity(const struct stat *left, const struct stat *right) {
    return left->st_dev == right->st_dev && left->st_ino == right->st_ino &&
           left->st_mode == right->st_mode && left->st_size == right->st_size &&
           left->st_mtimespec.tv_sec == right->st_mtimespec.tv_sec &&
           left->st_mtimespec.tv_nsec == right->st_mtimespec.tv_nsec &&
           left->st_ctimespec.tv_sec == right->st_ctimespec.tv_sec &&
           left->st_ctimespec.tv_nsec == right->st_ctimespec.tv_nsec;
}

static BOOL GPTPathStillNamesArtifact(const char *path, const struct stat *expected) {
    int descriptor = GPTOpenPackagedArtifact(path);
    if (descriptor < 0) {
        return NO;
    }
    struct stat observed = {0};
    BOOL matches = fstat(descriptor, &observed) == 0 &&
                   GPTSameArtifactIdentity(expected, &observed);
    close(descriptor);
    return matches;
}

static BOOL GPTEntitlementIsTrue(NSDictionary *entitlements, NSString *key) {
    id value = entitlements[key];
    return [value isKindOfClass:[NSNumber class]] && [value boolValue];
}

static BOOL GPTEntitlementIsAbsentOrFalse(NSDictionary *entitlements, NSString *key) {
    id value = entitlements[key];
    return value == nil || ([value isKindOfClass:[NSNumber class]] && ![value boolValue]);
}

static NSString *GPTValidatePackagedCode(
    NSURL *url,
    NSString *expectedIdentifier,
    NSString *expectedTeamIdentifier,
    BOOL checkNestedCode,
    BOOL requireVirtualization,
    BOOL forbidVirtualization,
    NSString *__autoreleasing *teamIdentifier,
    NSData *__autoreleasing *designatedRequirementData) {
    SecStaticCodeRef code = NULL;
    OSStatus status = SecStaticCodeCreateWithPath(
        (__bridge CFURLRef)url,
        kSecCSDefaultFlags,
        &code);
    if (status != errSecSuccess || code == NULL) {
        return @"Packaged isolated code could not be opened for signature validation";
    }

    SecCSFlags validationFlags = kSecCSCheckAllArchitectures | kSecCSStrictValidate |
                                 kSecCSRestrictSymlinks | kSecCSRestrictSidebandData;
    if (checkNestedCode) {
        validationFlags |= kSecCSCheckNestedCode;
    }
    CFErrorRef validationError = NULL;
    status = SecStaticCodeCheckValidityWithErrors(
        code,
        validationFlags,
        NULL,
        &validationError);
    if (validationError != NULL) {
        CFRelease(validationError);
    }
    if (status != errSecSuccess) {
        CFRelease(code);
        return @"Packaged isolated code failed strict offline signature validation";
    }

    CFDictionaryRef rawInformation = NULL;
    status = SecCodeCopySigningInformation(
        code,
        kSecCSSigningInformation,
        &rawInformation);
    if (status != errSecSuccess || rawInformation == NULL) {
        CFRelease(code);
        return @"Packaged isolated code has no usable signing information";
    }
    NSDictionary *information = (__bridge NSDictionary *)rawInformation;
    NSString *identifier = information[(__bridge NSString *)kSecCodeInfoIdentifier];
    NSString *observedTeam = information[(__bridge NSString *)kSecCodeInfoTeamIdentifier];
    NSNumber *signatureFlags = information[(__bridge NSString *)kSecCodeInfoFlags];
    NSDictionary *entitlements =
        information[(__bridge NSString *)kSecCodeInfoEntitlementsDict];
    if (![identifier isKindOfClass:[NSString class]] ||
        ![identifier isEqualToString:expectedIdentifier] ||
        ![observedTeam isKindOfClass:[NSString class]] || observedTeam.length == 0 ||
        (expectedTeamIdentifier != nil &&
         ![observedTeam isEqualToString:expectedTeamIdentifier]) ||
        ![signatureFlags isKindOfClass:[NSNumber class]]) {
        CFRelease(rawInformation);
        CFRelease(code);
        return @"Packaged isolated code has the wrong signing identity";
    }
    uint32_t flags = signatureFlags.unsignedIntValue;
    if ((flags & kSecCodeSignatureRuntime) == 0 ||
        (flags & kSecCodeSignatureLibraryValidation) == 0 ||
        (flags & kSecCodeSignatureAdhoc) != 0) {
        CFRelease(rawInformation);
        CFRelease(code);
        return @"Packaged isolated code is not a hardened non-ad-hoc signature";
    }
    if (entitlements != nil && ![entitlements isKindOfClass:[NSDictionary class]]) {
        CFRelease(rawInformation);
        CFRelease(code);
        return @"Packaged isolated code has malformed entitlements";
    }
    NSDictionary *checkedEntitlements = entitlements ?: @{};
    BOOL virtualization = GPTEntitlementIsTrue(
        checkedEntitlements,
        @"com.apple.security.virtualization");
    BOOL networkingAbsent = GPTEntitlementIsAbsentOrFalse(
        checkedEntitlements,
        @"com.apple.vm.networking");
    BOOL debugAttachmentAbsent = GPTEntitlementIsAbsentOrFalse(
        checkedEntitlements,
        @"com.apple.security.get-task-allow");
    if ((requireVirtualization && !virtualization) ||
        (forbidVirtualization && virtualization) || !networkingAbsent ||
        !debugAttachmentAbsent) {
        CFRelease(rawInformation);
        CFRelease(code);
        return @"Packaged isolated code violates the virtualization entitlement boundary";
    }
    if (requireVirtualization) {
        if (!GPTEntitlementIsTrue(
                checkedEntitlements,
                @"com.apple.security.app-sandbox")) {
            CFRelease(rawInformation);
            CFRelease(code);
            return @"Packaged isolated helper is not sandboxed";
        }
        NSSet<NSString *> *allowedHelperEntitlements = [NSSet setWithArray:@[
            @"com.apple.application-identifier",
            @"com.apple.developer.team-identifier",
            @"com.apple.security.app-sandbox",
            @"com.apple.security.virtualization",
        ]];
        for (id key in checkedEntitlements) {
            if (![key isKindOfClass:[NSString class]] ||
                ![allowedHelperEntitlements containsObject:key]) {
                CFRelease(rawInformation);
                CFRelease(code);
                return @"Packaged isolated helper requests an unreviewed entitlement";
            }
        }
        NSString *applicationIdentifier =
            checkedEntitlements[@"com.apple.application-identifier"];
        NSString *entitlementTeam =
            checkedEntitlements[@"com.apple.developer.team-identifier"];
        NSString *expectedApplicationIdentifier = [NSString
            stringWithFormat:@"%@.%@", observedTeam, expectedIdentifier];
        BOOL applicationIdentifierMatches =
            applicationIdentifier == nil ||
            ([applicationIdentifier isKindOfClass:[NSString class]] &&
             [applicationIdentifier isEqualToString:expectedApplicationIdentifier]);
        BOOL entitlementTeamMatches =
            entitlementTeam == nil ||
            ([entitlementTeam isKindOfClass:[NSString class]] &&
             [entitlementTeam isEqualToString:observedTeam]);
        if (!applicationIdentifierMatches || !entitlementTeamMatches) {
            CFRelease(rawInformation);
            CFRelease(code);
            return @"Packaged isolated helper has inconsistent sandbox identity entitlements";
        }
    }
    if (teamIdentifier != NULL) {
        *teamIdentifier = [observedTeam copy];
    }

    if (designatedRequirementData != NULL) {
        SecRequirementRef requirement = NULL;
        CFDataRef rawRequirementData = NULL;
        status = SecCodeCopyDesignatedRequirement(
            code,
            kSecCSDefaultFlags,
            &requirement);
        if (status == errSecSuccess && requirement != NULL) {
            status = SecRequirementCopyData(
                requirement,
                kSecCSDefaultFlags,
                &rawRequirementData);
        }
        if (requirement != NULL) {
            CFRelease(requirement);
        }
        if (status != errSecSuccess || rawRequirementData == NULL ||
            CFDataGetLength(rawRequirementData) <= 0 ||
            (size_t)CFDataGetLength(rawRequirementData) > GPT_MAX_REQUIREMENT_DATA_BYTES) {
            if (rawRequirementData != NULL) {
                CFRelease(rawRequirementData);
            }
            CFRelease(rawInformation);
            CFRelease(code);
            return @"Packaged helper has no bounded designated requirement";
        }
        *designatedRequirementData = [(__bridge NSData *)rawRequirementData copy];
        CFRelease(rawRequirementData);
    }
    CFRelease(rawInformation);
    CFRelease(code);
    return nil;
}

GPTMacIsolatedArtifactsResult gpt_macos_isolated_artifacts_open(void) {
    @autoreleasepool {
        NSBundle *bundle = NSBundle.mainBundle;
        if (![bundle.bundleIdentifier isEqualToString:GPT_APP_BUNDLE_IDENTIFIER]) {
            return GPTIsolatedArtifactsError(
                GPT_MAC_BACKEND_UNAVAILABLE,
                @"The isolated environment is not running from the GrokPtah application bundle");
        }
        const char *bundlePath = bundle.bundleURL.path.fileSystemRepresentation;
        char bundleRoot[PATH_MAX] = {0};
        if (bundlePath == NULL || realpath(bundlePath, bundleRoot) == NULL) {
            return GPTIsolatedArtifactsError(
                GPT_MAC_BACKEND_UNAVAILABLE,
                @"The GrokPtah application bundle could not be resolved");
        }

        char helperPath[PATH_MAX] = {0};
        char guestImagePath[PATH_MAX] = {0};
        char configurationPath[PATH_MAX] = {0};
        if (!GPTAppendBundlePath(
                bundleRoot,
                "Contents/MacOS/grokptah-isolated-visual-helper",
                helperPath) ||
            !GPTAppendBundlePath(
                bundleRoot,
                "Contents/Resources/isolated-visual/grokptah-isolated-guest-v1.img",
                guestImagePath) ||
            !GPTAppendBundlePath(
                bundleRoot,
                "Contents/Resources/isolated-visual/grokptah-isolated-config-v1.json",
                configurationPath)) {
            return GPTIsolatedArtifactsError(
                GPT_MAC_BACKEND_UNAVAILABLE,
                @"The isolated environment bundle layout is invalid");
        }

        int helperFD = GPTOpenPackagedArtifact(helperPath);
        int guestImageFD = GPTOpenPackagedArtifact(guestImagePath);
        int configurationFD = GPTOpenPackagedArtifact(configurationPath);
        if (helperFD < 0 || guestImageFD < 0 || configurationFD < 0) {
            GPTCloseArtifactDescriptors(helperFD, guestImageFD, configurationFD);
            return GPTIsolatedArtifactsError(
                GPT_MAC_BACKEND_UNAVAILABLE,
                @"The signed helper and measured guest artifacts are not packaged");
        }
        struct stat helperIdentity = {0};
        struct stat guestImageIdentity = {0};
        struct stat configurationIdentity = {0};
        if (fstat(helperFD, &helperIdentity) != 0 ||
            fstat(guestImageFD, &guestImageIdentity) != 0 ||
            fstat(configurationFD, &configurationIdentity) != 0 ||
            !S_ISREG(helperIdentity.st_mode) || !S_ISREG(guestImageIdentity.st_mode) ||
            !S_ISREG(configurationIdentity.st_mode)) {
            GPTCloseArtifactDescriptors(helperFD, guestImageFD, configurationFD);
            return GPTIsolatedArtifactsError(
                GPT_MAC_FORBIDDEN_ACTION,
                @"The isolated environment contains an invalid artifact type");
        }

        NSURL *bundleURL = [NSURL fileURLWithFileSystemRepresentation:bundleRoot
                                                          isDirectory:YES
                                                        relativeToURL:nil];
        NSString *teamIdentifier = nil;
        NSString *validationFailure = GPTValidatePackagedCode(
            bundleURL,
            GPT_APP_BUNDLE_IDENTIFIER,
            nil,
            YES,
            NO,
            YES,
            &teamIdentifier,
            NULL);
        if (validationFailure != nil) {
            GPTCloseArtifactDescriptors(helperFD, guestImageFD, configurationFD);
            return GPTIsolatedArtifactsError(GPT_MAC_UNAUTHORIZED, validationFailure);
        }

        NSURL *helperURL = [NSURL fileURLWithFileSystemRepresentation:helperPath
                                                           isDirectory:NO
                                                         relativeToURL:nil];
        NSData *requirementData = nil;
        validationFailure = GPTValidatePackagedCode(
            helperURL,
            GPT_HELPER_SIGNING_IDENTIFIER,
            teamIdentifier,
            NO,
            YES,
            NO,
            NULL,
            &requirementData);
        if (validationFailure != nil) {
            GPTCloseArtifactDescriptors(helperFD, guestImageFD, configurationFD);
            return GPTIsolatedArtifactsError(GPT_MAC_UNAUTHORIZED, validationFailure);
        }
        if (!GPTPathStillNamesArtifact(helperPath, &helperIdentity) ||
            !GPTPathStillNamesArtifact(guestImagePath, &guestImageIdentity) ||
            !GPTPathStillNamesArtifact(configurationPath, &configurationIdentity)) {
            GPTCloseArtifactDescriptors(helperFD, guestImageFD, configurationFD);
            return GPTIsolatedArtifactsError(
                GPT_MAC_TARGET_CHANGED,
                @"An isolated environment artifact changed during package verification");
        }

        GPTMacIsolatedArtifactsResult result =
            GPTEmptyIsolatedArtifactsResult(GPT_MAC_OK);
        result.helper_fd = helperFD;
        result.guest_image_fd = guestImageFD;
        result.configuration_fd = configurationFD;
        result.requirement_data = GPTCopyBytes(
            requirementData,
            &result.requirement_data_len);
        if (result.requirement_data == NULL || result.requirement_data_len == 0) {
            GPTCloseArtifactDescriptors(helperFD, guestImageFD, configurationFD);
            free(result.requirement_data);
            return GPTIsolatedArtifactsError(
                GPT_MAC_BACKEND_FAILURE,
                @"The packaged helper requirement could not be retained");
        }
        return result;
    }
}

void gpt_macos_isolated_artifacts_result_free(GPTMacIsolatedArtifactsResult *result) {
    if (result == NULL) {
        return;
    }
    free(result->requirement_data);
    free(result->error);
    result->requirement_data = NULL;
    result->requirement_data_len = 0;
    result->error = NULL;
    result->error_len = 0;
}

static GPTMacIsolatedRuntimeSpawnResult GPTEmptyIsolatedRuntimeSpawnResult(int32_t status) {
    GPTMacIsolatedRuntimeSpawnResult result = {0};
    result.status = status;
    result.pid = -1;
    result.control_fd = -1;
    result.event_fd = -1;
    result.input_fd = -1;
    result.frame_fd = -1;
    result.challenge_fd = -1;
    return result;
}

static GPTMacIsolatedRuntimeSpawnResult GPTIsolatedRuntimeSpawnError(
    int32_t status,
    NSString *message) {
    GPTMacIsolatedRuntimeSpawnResult result = GPTEmptyIsolatedRuntimeSpawnResult(status);
    NSString *bounded = message.length > 512 ? [message substringToIndex:512] : message;
    NSData *data = [bounded dataUsingEncoding:NSUTF8StringEncoding];
    result.error = GPTCopyBytes(data, &result.error_len);
    return result;
}

void gpt_macos_isolated_runtime_spawn_result_free(GPTMacIsolatedRuntimeSpawnResult *result) {
    if (result == NULL) {
        return;
    }
    free(result->error);
    result->error = NULL;
    result->error_len = 0;
}

static int GPTAddCloseIfDistinct(
    posix_spawn_file_actions_t *actions,
    int descriptor,
    int target) {
    if (descriptor < 0 || descriptor == target) {
        return 0;
    }
    return posix_spawn_file_actions_addclose(actions, descriptor);
}

static BOOL GPTSetCloseOnExec(int descriptor) {
    int flags = fcntl(descriptor, F_GETFD);
    return flags >= 0 && fcntl(descriptor, F_SETFD, flags | FD_CLOEXEC) == 0;
}

static BOOL GPTIsCloseOnExec(int descriptor) {
    int flags = fcntl(descriptor, F_GETFD);
    return flags >= 0 && (flags & FD_CLOEXEC) != 0;
}

static int GPTCreateCloseOnExecPipe(int pair[2]) {
    pair[0] = -1;
    pair[1] = -1;
    if (pipe(pair) != 0 || !GPTSetCloseOnExec(pair[0]) || !GPTSetCloseOnExec(pair[1])) {
        close(pair[0]);
        close(pair[1]);
        pair[0] = -1;
        pair[1] = -1;
        return -1;
    }
    return 0;
}

GPTMacIsolatedRuntimeSpawnResult gpt_macos_isolated_runtime_spawn(
    int32_t helper_fd,
    int32_t guest_image_fd,
    int32_t configuration_fd) {
    @autoreleasepool {
#ifndef POSIX_SPAWN_CLOEXEC_DEFAULT
        return GPTIsolatedRuntimeSpawnError(
            GPT_MAC_BACKEND_UNAVAILABLE,
            @"The macOS SDK does not provide close-on-exec spawn isolation");
#else
        if (helper_fd < 0 || guest_image_fd < 0 || configuration_fd < 0) {
            return GPTIsolatedRuntimeSpawnError(
                GPT_MAC_INVALID_REQUEST,
                @"The measured isolated artifact descriptors are incomplete");
        }

        NSBundle *bundle = NSBundle.mainBundle;
        const char *bundlePath = bundle.bundleURL.path.fileSystemRepresentation;
        char bundleRoot[PATH_MAX] = {0};
        char helperPath[PATH_MAX] = {0};
        struct stat helperIdentity = {0};
        struct stat guestImageIdentity = {0};
        struct stat configurationIdentity = {0};
        if (bundlePath == NULL || realpath(bundlePath, bundleRoot) == NULL ||
            !GPTAppendBundlePath(
                bundleRoot,
                "Contents/MacOS/grokptah-isolated-visual-helper",
                helperPath) ||
            fstat(helper_fd, &helperIdentity) != 0 ||
            fstat(guest_image_fd, &guestImageIdentity) != 0 ||
            fstat(configuration_fd, &configurationIdentity) != 0 ||
            !S_ISREG(helperIdentity.st_mode) ||
            !S_ISREG(guestImageIdentity.st_mode) ||
            !S_ISREG(configurationIdentity.st_mode) ||
            !GPTIsCloseOnExec(helper_fd) ||
            !GPTIsCloseOnExec(guest_image_fd) ||
            !GPTIsCloseOnExec(configuration_fd) ||
            !GPTPathStillNamesArtifact(helperPath, &helperIdentity)) {
            return GPTIsolatedRuntimeSpawnError(
                GPT_MAC_FORBIDDEN_ACTION,
                @"Measured isolated artifact descriptors are not close-on-exec");
        }

        int control[2] = {-1, -1};
        int events[2] = {-1, -1};
        int input[2] = {-1, -1};
        int frames[2] = {-1, -1};
        int challenge[2] = {-1, -1};
        int null_fd = -1;
        if (GPTCreateCloseOnExecPipe(control) != 0 ||
            GPTCreateCloseOnExecPipe(events) != 0 ||
            GPTCreateCloseOnExecPipe(input) != 0 ||
            GPTCreateCloseOnExecPipe(frames) != 0 ||
            GPTCreateCloseOnExecPipe(challenge) != 0 ||
            (null_fd = open("/dev/null", O_RDWR | O_CLOEXEC)) < 0) {
            close(control[0]);
            close(control[1]);
            close(events[0]);
            close(events[1]);
            close(input[0]);
            close(input[1]);
            close(frames[0]);
            close(frames[1]);
            close(challenge[0]);
            close(challenge[1]);
            close(null_fd);
            return GPTIsolatedRuntimeSpawnError(
                GPT_MAC_BACKEND_FAILURE,
                @"Could not allocate isolated helper pipes");
        }

        posix_spawn_file_actions_t actions;
        posix_spawnattr_t attributes;
        int action_status = posix_spawn_file_actions_init(&actions);
        BOOL actions_initialized = action_status == 0;
        BOOL attributes_initialized = NO;
        if (action_status == 0) {
            action_status = posix_spawnattr_init(&attributes);
            attributes_initialized = action_status == 0;
        }
        short flags = POSIX_SPAWN_CLOEXEC_DEFAULT;
        if (action_status == 0) {
            action_status = posix_spawnattr_setflags(&attributes, flags);
        }
        int sources[] = {
            helper_fd,
            guest_image_fd,
            configuration_fd,
            control[0],
            control[1],
            events[0],
            events[1],
            input[0],
            input[1],
            frames[0],
            frames[1],
            challenge[0],
            challenge[1],
            null_fd,
        };
        int inherited_targets[] = {3, 4, 5, 6, 7, 8, GPT_CHALLENGE_FD};
        int standard_targets[] = {0, 1, 2};
        if (action_status == 0) {
            action_status = posix_spawn_file_actions_adddup2(
                &actions,
                guest_image_fd,
                3);
        }
        if (action_status == 0) {
            action_status = posix_spawn_file_actions_adddup2(
                &actions,
                configuration_fd,
                4);
        }
        if (action_status == 0) {
            action_status = posix_spawn_file_actions_adddup2(&actions, control[0], 5);
        }
        if (action_status == 0) {
            action_status = posix_spawn_file_actions_adddup2(&actions, events[1], 6);
        }
        if (action_status == 0) {
            action_status = posix_spawn_file_actions_adddup2(&actions, input[0], 7);
        }
        if (action_status == 0) {
            action_status = posix_spawn_file_actions_adddup2(&actions, frames[1], 8);
        }
        if (action_status == 0) {
            action_status = posix_spawn_file_actions_adddup2(
                &actions,
                challenge[1],
                GPT_CHALLENGE_FD);
        }
        for (size_t index = 0;
             action_status == 0 && index < sizeof(standard_targets) / sizeof(standard_targets[0]);
             ++index) {
            action_status = posix_spawn_file_actions_adddup2(
                &actions,
                null_fd,
                standard_targets[index]);
        }
        for (size_t index = 0; action_status == 0 && index < sizeof(sources) / sizeof(sources[0]);
             ++index) {
            int target = -1;
            for (size_t target_index = 0;
                 target_index < sizeof(inherited_targets) / sizeof(inherited_targets[0]);
                 ++target_index) {
                if (sources[index] == inherited_targets[target_index]) {
                    target = inherited_targets[target_index];
                    break;
                }
            }
            action_status = GPTAddCloseIfDistinct(&actions, sources[index], target);
        }

        pid_t pid = -1;
        char *arguments[] = {helperPath, NULL};
        char *environment[] = {NULL};
        if (action_status == 0) {
            action_status = posix_spawn(
                &pid,
                helperPath,
                &actions,
                &attributes,
                arguments,
                environment);
        }
        if (attributes_initialized) {
            posix_spawnattr_destroy(&attributes);
        }
        if (actions_initialized) {
            posix_spawn_file_actions_destroy(&actions);
        }
        close(null_fd);
        close(control[0]);
        close(events[1]);
        close(input[0]);
        close(frames[1]);
        close(challenge[1]);
        if (action_status != 0 || pid < 0) {
            close(control[1]);
            close(events[0]);
            close(input[1]);
            close(frames[0]);
            close(challenge[0]);
            return GPTIsolatedRuntimeSpawnError(
                GPT_MAC_BACKEND_FAILURE,
                [NSString stringWithFormat:@"Isolated helper spawn failed (%d)", action_status]);
        }
        GPTMacIsolatedRuntimeSpawnResult result =
            GPTEmptyIsolatedRuntimeSpawnResult(GPT_MAC_OK);
        result.pid = (int32_t)pid;
        result.control_fd = control[1];
        result.event_fd = events[0];
        result.input_fd = input[1];
        result.frame_fd = frames[0];
        result.challenge_fd = challenge[0];
        return result;
#endif
    }
}

bool gpt_macos_screen_recording_granted(void) {
    return CGPreflightScreenCaptureAccess();
}

bool gpt_macos_request_screen_recording(void) {
    return CGRequestScreenCaptureAccess();
}

bool gpt_macos_accessibility_granted(void) {
    return AXIsProcessTrusted();
}

bool gpt_macos_request_accessibility(void) {
    NSDictionary *options = @{(__bridge NSString *)kAXTrustedCheckOptionPrompt : @YES};
    return AXIsProcessTrustedWithOptions((__bridge CFDictionaryRef)options);
}

static SCShareableContent *GPTShareableContent(void) API_AVAILABLE(macos(14.0)) {
    // The bridge smoke can run as a non-AppKit CLI; the packaged desktop app
    // already has NSApplication initialized, so this is a no-op there.
    if (NSApp == nil) {
        (void)NSApplicationLoad();
    }
    dispatch_semaphore_t semaphore = dispatch_semaphore_create(0);
    __block SCShareableContent *captured = nil;
    Class shareableContentClass = NSClassFromString(@"SCShareableContent");
    [shareableContentClass
        getShareableContentExcludingDesktopWindows:YES
                              onScreenWindowsOnly:NO
                                completionHandler:^(SCShareableContent *content, NSError *error) {
                                    (void)error;
                                    captured = content;
                                    dispatch_semaphore_signal(semaphore);
                                }];
    long wait = dispatch_semaphore_wait(
        semaphore, dispatch_time(DISPATCH_TIME_NOW, (int64_t)(10 * NSEC_PER_SEC)));
    return wait == 0 ? captured : nil;
}

static BOOL GPTDeniedBundle(NSString *bundleID) {
    static NSSet<NSString *> *denied;
    static dispatch_once_t once;
    dispatch_once(&once, ^{
        denied = [NSSet setWithArray:@[
            @"com.apple.securityagent",
            @"com.apple.systemsettings",
            @"com.apple.authorizationhost",
            @"com.apple.controlcenter",
            @"com.apple.keychainaccess",
            @"com.apple.loginwindow",
            @"com.apple.notificationcenterui",
            @"com.apple.systempreferences",
            @"com.1password.1password",
            @"com.agilebits.onepassword7",
            @"com.bitwarden.desktop",
            @"com.chriscase.grokptah",
            @"com.dashlane.dashlane",
            @"com.lastpass.lastpass",
            @"org.keepassxc.keepassxc",
        ]];
    });
    return bundleID == nil || [denied containsObject:bundleID.lowercaseString];
}

static NSDictionary *GPTFrameDictionary(CGRect frame) {
    return @{
        @"x" : @(frame.origin.x),
        @"y" : @(frame.origin.y),
        @"width" : @(frame.size.width),
        @"height" : @(frame.size.height),
        @"scaleFactor" : @1.0,
    };
}

static BOOL GPTUsableWindow(SCWindow *window) API_AVAILABLE(macos(14.0)) {
    SCRunningApplication *application = window.owningApplication;
    CGRect frame = window.frame;
    return application != nil && application.bundleIdentifier.length > 0 &&
           !GPTDeniedBundle(application.bundleIdentifier) &&
           window.windowID != 0 && window.windowLayer == 0 && isfinite(frame.origin.x) &&
           isfinite(frame.origin.y) && isfinite(frame.size.width) &&
           isfinite(frame.size.height) && frame.size.width >= 4.0 &&
           frame.size.height >= 4.0 && frame.size.width <= 100000.0 &&
           frame.size.height <= 100000.0;
}

static NSDictionary *GPTWindowDictionary(SCWindow *window) API_AVAILABLE(macos(14.0)) {
    SCRunningApplication *application = window.owningApplication;
    BOOL active = window.isActive;
    BOOL onScreen = window.isOnScreen;
    return @{
        @"windowId" : @(window.windowID),
        @"processId" : @(application.processID),
        @"bundleId" : application.bundleIdentifier,
        @"applicationName" : application.applicationName,
        @"frame" : GPTFrameDictionary(window.frame),
        @"onScreen" : @(onScreen),
        @"active" : @(active),
        // Box the expression as BOOL so NSJSONSerialization emits JSON true/false.
        @"minimized" : @((BOOL)(!onScreen && !active)),
    };
}

static SCWindow *GPTFindWindow(
    SCShareableContent *content,
    uint32_t windowID,
    int32_t processID,
    NSString *bundleID) API_AVAILABLE(macos(14.0)) {
    for (SCWindow *window in content.windows) {
        SCRunningApplication *application = window.owningApplication;
        if (window.windowID == windowID && application.processID == processID &&
            [application.bundleIdentifier isEqualToString:bundleID] && GPTUsableWindow(window)) {
            return window;
        }
    }
    return nil;
}

GPTMacNativeResult gpt_macos_list_targets(void) {
    @autoreleasepool {
        if (!gpt_macos_observation_supported()) {
            return GPTErrorResult(GPT_MAC_UNSUPPORTED, @"macOS 14 or later is required");
        }
        if (!CGPreflightScreenCaptureAccess()) {
            return GPTErrorResult(
                GPT_MAC_PERMISSION_REQUIRED, @"Screen Recording permission is required");
        }
        if (@available(macOS 14.0, *)) {
            SCShareableContent *content = GPTShareableContent();
            if (content == nil) {
                return GPTErrorResult(
                    GPT_MAC_BACKEND_FAILURE, @"macOS could not enumerate shareable windows");
            }
            NSMutableArray *targets = [NSMutableArray array];
            for (SCWindow *window in content.windows) {
                if (GPTUsableWindow(window)) {
                    [targets addObject:GPTWindowDictionary(window)];
                    if (targets.count == 256) {
                        break;
                    }
                }
            }
            return GPTJSONResult(targets, nil);
        }
        return GPTErrorResult(GPT_MAC_UNSUPPORTED, @"macOS 14 or later is required");
    }
}

static NSString *GPTCopyAXString(AXUIElementRef element, CFStringRef attribute) {
    CFTypeRef value = NULL;
    if (AXUIElementCopyAttributeValue(element, attribute, &value) != kAXErrorSuccess ||
        value == NULL) {
        return nil;
    }
    NSString *result = nil;
    if (CFGetTypeID(value) == CFStringGetTypeID()) {
        result = [(__bridge NSString *)value copy];
    }
    CFRelease(value);
    return result;
}

static NSNumber *GPTCopyAXBool(AXUIElementRef element, CFStringRef attribute) {
    CFTypeRef value = NULL;
    if (AXUIElementCopyAttributeValue(element, attribute, &value) != kAXErrorSuccess ||
        value == NULL) {
        return nil;
    }
    NSNumber *result = nil;
    if (CFGetTypeID(value) == CFBooleanGetTypeID()) {
        result = @((BOOL)CFBooleanGetValue((CFBooleanRef)value));
    }
    CFRelease(value);
    return result;
}

static NSString *GPTCopyAXScalarDescription(AXUIElementRef element, CFStringRef attribute) {
    CFTypeRef value = NULL;
    if (AXUIElementCopyAttributeValue(element, attribute, &value) != kAXErrorSuccess ||
        value == NULL) {
        return nil;
    }
    NSString *result = nil;
    CFTypeID type = CFGetTypeID(value);
    if (type == CFStringGetTypeID() || type == CFNumberGetTypeID() ||
        type == CFBooleanGetTypeID()) {
        result = [[(__bridge id)value description] copy];
        if (result.length > 512) {
            result = [result substringToIndex:512];
        }
    }
    CFRelease(value);
    return result;
}

static BOOL GPTCopyAXFrame(AXUIElementRef element, CGRect *frame) {
    CFTypeRef positionValue = NULL;
    CFTypeRef sizeValue = NULL;
    CGPoint position = CGPointZero;
    CGSize size = CGSizeZero;
    AXError positionError = AXUIElementCopyAttributeValue(
        element, kAXPositionAttribute, &positionValue);
    AXError sizeError = AXUIElementCopyAttributeValue(element, kAXSizeAttribute, &sizeValue);
    BOOL valid = positionError == kAXErrorSuccess && sizeError == kAXErrorSuccess &&
                 positionValue != NULL && sizeValue != NULL &&
                 CFGetTypeID(positionValue) == AXValueGetTypeID() &&
                 CFGetTypeID(sizeValue) == AXValueGetTypeID() &&
                 AXValueGetValue((AXValueRef)positionValue, kAXValueCGPointType, &position) &&
                 AXValueGetValue((AXValueRef)sizeValue, kAXValueCGSizeType, &size) &&
                 isfinite(position.x) && isfinite(position.y) && isfinite(size.width) &&
                 isfinite(size.height) && size.width > 0.0 && size.height > 0.0;
    if (positionValue != NULL) {
        CFRelease(positionValue);
    }
    if (sizeValue != NULL) {
        CFRelease(sizeValue);
    }
    if (valid) {
        *frame = (CGRect){position, size};
    }
    return valid;
}

static AXUIElementRef GPTCopyMatchingAXWindow(pid_t processID, CGRect expectedFrame) {
    AXUIElementRef application = AXUIElementCreateApplication(processID);
    if (application == NULL) {
        return NULL;
    }
    CFTypeRef windowsValue = NULL;
    AXError error = AXUIElementCopyAttributeValue(
        application, kAXWindowsAttribute, &windowsValue);
    CFRelease(application);
    if (error != kAXErrorSuccess || windowsValue == NULL ||
        CFGetTypeID(windowsValue) != CFArrayGetTypeID()) {
        if (windowsValue != NULL) {
            CFRelease(windowsValue);
        }
        return NULL;
    }
    CFArrayRef windows = (CFArrayRef)windowsValue;
    AXUIElementRef best = NULL;
    CGFloat bestScore = CGFLOAT_MAX;
    NSUInteger bestCount = 0;
    for (CFIndex index = 0; index < CFArrayGetCount(windows); index++) {
        AXUIElementRef window = (AXUIElementRef)CFArrayGetValueAtIndex(windows, index);
        CGRect frame = CGRectZero;
        if (!GPTCopyAXFrame(window, &frame)) {
            continue;
        }
        CGFloat score = fabs(frame.origin.x - expectedFrame.origin.x) +
                        fabs(frame.origin.y - expectedFrame.origin.y) +
                        fabs(frame.size.width - expectedFrame.size.width) +
                        fabs(frame.size.height - expectedFrame.size.height);
        if (score < bestScore - 0.001) {
            bestScore = score;
            best = window;
            bestCount = 1;
        } else if (fabs(score - bestScore) <= 0.001) {
            bestCount += 1;
        }
    }
    // Fail closed when Accessibility cannot identify exactly one surface for
    // the ScreenCaptureKit window. A broad nearest-window match can redirect
    // semantic data to a different same-process window.
    if (best != NULL && bestScore <= 8.0 && bestCount == 1) {
        CFRetain(best);
    } else {
        best = NULL;
    }
    CFRelease(windowsValue);
    return best;
}

static NSArray<NSString *> *GPTAXActions(AXUIElementRef element, NSString *role) {
    NSMutableOrderedSet<NSString *> *actions = [NSMutableOrderedSet orderedSet];
    CFArrayRef actionNames = NULL;
    if (AXUIElementCopyActionNames(element, &actionNames) == kAXErrorSuccess &&
        actionNames != NULL) {
        for (NSString *action in (__bridge NSArray *)actionNames) {
            if ([action isEqualToString:(__bridge NSString *)kAXPressAction] ||
                [action isEqualToString:(__bridge NSString *)kAXConfirmAction]) {
                [actions addObject:@"invoke"];
            }
            if ([action isEqualToString:(__bridge NSString *)kAXShowMenuAction]) {
                [actions addObject:@"select"];
            }
            // AXScrollToVisible is emitted by some applications but is not
            // declared by every macOS SDK that GrokPtah supports.
            if ([action isEqualToString:@"AXScrollToVisible"]) {
                [actions addObject:@"scroll"];
            }
        }
        CFRelease(actionNames);
    }
    Boolean settable = false;
    if (AXUIElementIsAttributeSettable(element, kAXValueAttribute, &settable) == kAXErrorSuccess &&
        settable) {
        [actions addObject:@"set_value"];
    }
    if ([role isEqualToString:(__bridge NSString *)kAXRowRole] ||
        [role isEqualToString:(__bridge NSString *)kAXMenuItemRole]) {
        [actions addObject:@"select"];
    }
    return actions.array;
}

static NSString *GPTAXLabel(AXUIElementRef element) {
    for (NSString *attribute in @[
             (__bridge NSString *)kAXTitleAttribute,
             (__bridge NSString *)kAXDescriptionAttribute,
             (__bridge NSString *)kAXPlaceholderValueAttribute,
         ]) {
        NSString *label = GPTCopyAXString(element, (__bridge CFStringRef)attribute);
        if (label.length > 0) {
            return label.length > 512 ? [label substringToIndex:512] : label;
        }
    }
    return nil;
}

static void GPTTraverseAX(
    AXUIElementRef element,
    NSUInteger depth,
    NSUInteger maxNodes,
    NSMutableArray<NSDictionary *> *nodes,
    NSMutableArray<NSValue *> *secureFrames,
    NSMutableSet<NSValue *> *visited,
    BOOL *truncated) {
    if (depth > GPT_MAX_AX_DEPTH || nodes.count >= maxNodes || visited.count >= maxNodes) {
        *truncated = YES;
        return;
    }
    NSValue *identity = [NSValue valueWithPointer:(const void *)element];
    if ([visited containsObject:identity]) {
        return;
    }
    [visited addObject:identity];

    NSString *role = GPTCopyAXString(element, kAXRoleAttribute);
    NSString *subrole = GPTCopyAXString(element, kAXSubroleAttribute);
    BOOL secure = [role localizedCaseInsensitiveContainsString:@"secure"] ||
                  [subrole localizedCaseInsensitiveContainsString:@"secure"];
    CGRect frame = CGRectZero;
    BOOL hasFrame = GPTCopyAXFrame(element, &frame);
    if (secure && hasFrame) {
        [secureFrames addObject:[NSValue valueWithRect:NSRectFromCGRect(frame)]];
    }
    if (role.length > 0) {
        NSMutableDictionary *node = [NSMutableDictionary dictionary];
        node[@"role"] = role;
        if (subrole.length > 0) {
            node[@"subrole"] = subrole;
        }
        NSString *label = GPTAXLabel(element);
        if (label.length > 0) {
            node[@"label"] = label;
        }
        if (!secure) {
            NSString *value = GPTCopyAXScalarDescription(element, kAXValueAttribute);
            if (value.length > 0) {
                node[@"value"] = value;
            }
        }
        if (hasFrame) {
            node[@"frame"] = GPTFrameDictionary(frame);
        }
        node[@"enabled"] = GPTCopyAXBool(element, kAXEnabledAttribute) ?: @YES;
        node[@"focused"] = GPTCopyAXBool(element, kAXFocusedAttribute) ?: @NO;
        node[@"sensitivity"] = secure ? @"secure" : @"none";
        node[@"actions"] = GPTAXActions(element, role);
        [nodes addObject:node];
    }

    CFTypeRef childrenValue = NULL;
    AXError childrenError =
        AXUIElementCopyAttributeValue(element, kAXChildrenAttribute, &childrenValue);
    if (childrenError == kAXErrorSuccess && childrenValue != NULL &&
        CFGetTypeID(childrenValue) == CFArrayGetTypeID()) {
        CFArrayRef children = (CFArrayRef)childrenValue;
        for (CFIndex index = 0; index < CFArrayGetCount(children); index++) {
            if (nodes.count >= maxNodes) {
                *truncated = YES;
                break;
            }
            AXUIElementRef child = (AXUIElementRef)CFArrayGetValueAtIndex(children, index);
            GPTTraverseAX(
                child, depth + 1, maxNodes, nodes, secureFrames, visited, truncated);
        }
    } else if ((childrenError != kAXErrorNoValue &&
                childrenError != kAXErrorAttributeUnsupported) ||
               childrenError == kAXErrorSuccess) {
        // Unexpected or malformed partial trees cannot prove that every
        // secure field was found, so the Rust layer must withhold the image.
        *truncated = YES;
    }
    if (childrenValue != NULL) {
        CFRelease(childrenValue);
    }
}

static NSData *GPTCaptureRedactedWindow(
    SCWindow *window,
    CGRect windowFrame,
    NSArray<NSValue *> *secureFrames,
    uint32_t maxDimension,
    uint64_t maxPNGBytes,
    uint32_t *pixelWidth,
    uint32_t *pixelHeight,
    int32_t *status) API_AVAILABLE(macos(14.0)) {
    if (!isfinite(windowFrame.size.width) || !isfinite(windowFrame.size.height) ||
        windowFrame.size.width < 1.0 || windowFrame.size.height < 1.0 ||
        windowFrame.size.width > 100000.0 || windowFrame.size.height > 100000.0) {
        *status = GPT_MAC_BACKEND_FAILURE;
        return nil;
    }
    NSUInteger dimensionLimit = MIN(
        MAX((NSUInteger)maxDimension, (NSUInteger)1), GPT_MAX_NATIVE_SCREENSHOT_DIMENSION);
    CGFloat scale = 2.0;
    NSUInteger width = MAX((NSUInteger)1, (NSUInteger)ceil(windowFrame.size.width * scale));
    NSUInteger height = MAX((NSUInteger)1, (NSUInteger)ceil(windowFrame.size.height * scale));
    CGFloat downscale = MIN(1.0, MIN((CGFloat)dimensionLimit / width, (CGFloat)dimensionLimit / height));
    width = MAX((NSUInteger)1, (NSUInteger)floor(width * downscale));
    height = MAX((NSUInteger)1, (NSUInteger)floor(height * downscale));
    while (width > 1 && height > 1 && width * height * 4 > GPT_MAX_RAW_SCREENSHOT_BYTES) {
        width /= 2;
        height /= 2;
    }

    Class contentFilterClass = NSClassFromString(@"SCContentFilter");
    Class configurationClass = NSClassFromString(@"SCStreamConfiguration");
    Class screenshotManagerClass = NSClassFromString(@"SCScreenshotManager");
    if (contentFilterClass == Nil || configurationClass == Nil || screenshotManagerClass == Nil) {
        *status = GPT_MAC_UNSUPPORTED;
        return nil;
    }
    SCContentFilter *filter = [[contentFilterClass alloc] initWithDesktopIndependentWindow:window];
    SCStreamConfiguration *configuration = [[configurationClass alloc] init];
    configuration.width = width;
    configuration.height = height;
    configuration.showsCursor = NO;
    configuration.capturesAudio = NO;
    configuration.queueDepth = 1;

    dispatch_semaphore_t semaphore = dispatch_semaphore_create(0);
    __block CGImageRef capturedImage = NULL;
    [screenshotManagerClass
        captureImageWithFilter:filter
                 configuration:configuration
             completionHandler:^(CGImageRef image, NSError *error) {
                 (void)error;
                 if (image != NULL) {
                     capturedImage = CGImageRetain(image);
                 }
                 dispatch_semaphore_signal(semaphore);
             }];
    long wait = dispatch_semaphore_wait(
        semaphore, dispatch_time(DISPATCH_TIME_NOW, (int64_t)(10 * NSEC_PER_SEC)));
    if (wait != 0 || capturedImage == NULL) {
        if (capturedImage != NULL) {
            CGImageRelease(capturedImage);
        }
        *status = GPT_MAC_BACKEND_FAILURE;
        return nil;
    }

    width = CGImageGetWidth(capturedImage);
    height = CGImageGetHeight(capturedImage);
    if (width == 0 || height == 0 || width > dimensionLimit || height > dimensionLimit ||
        width * height * 4 > GPT_MAX_RAW_SCREENSHOT_BYTES) {
        CGImageRelease(capturedImage);
        *status = GPT_MAC_LIMIT_REACHED;
        return nil;
    }
    size_t bytesPerRow = width * 4;
    void *pixels = calloc(height, bytesPerRow);
    if (pixels == NULL) {
        CGImageRelease(capturedImage);
        *status = GPT_MAC_BACKEND_FAILURE;
        return nil;
    }
    CGColorSpaceRef colorSpace = CGColorSpaceCreateDeviceRGB();
    if (colorSpace == NULL) {
        free(pixels);
        CGImageRelease(capturedImage);
        *status = GPT_MAC_BACKEND_FAILURE;
        return nil;
    }
    CGContextRef context = CGBitmapContextCreate(
        pixels,
        width,
        height,
        8,
        bytesPerRow,
        colorSpace,
        kCGImageAlphaPremultipliedLast | kCGBitmapByteOrder32Big);
    CGColorSpaceRelease(colorSpace);
    if (context == NULL) {
        free(pixels);
        CGImageRelease(capturedImage);
        *status = GPT_MAC_BACKEND_FAILURE;
        return nil;
    }
    CGContextDrawImage(context, CGRectMake(0, 0, width, height), capturedImage);
    CGImageRelease(capturedImage);

    CGFloat scaleX = (CGFloat)width / windowFrame.size.width;
    CGFloat scaleY = (CGFloat)height / windowFrame.size.height;
    CGContextSetRGBFillColor(context, 0.0, 0.0, 0.0, 1.0);
    for (NSValue *value in secureFrames) {
        CGRect secure = NSRectToCGRect(value.rectValue);
        CGFloat x = (secure.origin.x - windowFrame.origin.x) * scaleX;
        CGFloat top = (secure.origin.y - windowFrame.origin.y) * scaleY;
        CGFloat secureWidth = secure.size.width * scaleX;
        CGFloat secureHeight = secure.size.height * scaleY;
        // AX and bitmap APIs can report opposite vertical origins depending
        // on display arrangement. Redact both conservative projections.
        CGFloat yCandidates[] = {top, (CGFloat)height - top - secureHeight};
        for (NSUInteger index = 0; index < 2; index++) {
            CGRect redaction = CGRectIntersection(
                CGRectMake(0, 0, width, height),
                CGRectInset(
                    CGRectMake(x, yCandidates[index], secureWidth, secureHeight), -4.0, -4.0));
            if (!CGRectIsNull(redaction) && !CGRectIsEmpty(redaction)) {
                CGContextFillRect(context, redaction);
            }
        }
    }

    CGImageRef redactedImage = CGBitmapContextCreateImage(context);
    CGContextRelease(context);
    free(pixels);
    if (redactedImage == NULL) {
        *status = GPT_MAC_BACKEND_FAILURE;
        return nil;
    }
    NSBitmapImageRep *representation = [[NSBitmapImageRep alloc] initWithCGImage:redactedImage];
    CGImageRelease(redactedImage);
    NSData *png = [representation representationUsingType:NSBitmapImageFileTypePNG properties:@{}];
    if (png == nil || png.length == 0 || png.length > maxPNGBytes) {
        *status = GPT_MAC_LIMIT_REACHED;
        return nil;
    }
    *pixelWidth = (uint32_t)width;
    *pixelHeight = (uint32_t)height;
    *status = GPT_MAC_OK;
    return png;
}

static BOOL GPTFrameMatches(CGRect first, CGRect second) {
    return fabs(first.origin.x - second.origin.x) <= 2.0 &&
           fabs(first.origin.y - second.origin.y) <= 2.0 &&
           fabs(first.size.width - second.size.width) <= 2.0 &&
           fabs(first.size.height - second.size.height) <= 2.0;
}

static BOOL GPTReadFrame(NSDictionary *dictionary, CGRect *frame) {
    if (![dictionary isKindOfClass:[NSDictionary class]]) {
        return NO;
    }
    NSNumber *x = dictionary[@"x"];
    NSNumber *y = dictionary[@"y"];
    NSNumber *width = dictionary[@"width"];
    NSNumber *height = dictionary[@"height"];
    if (![x isKindOfClass:[NSNumber class]] || ![y isKindOfClass:[NSNumber class]] ||
        ![width isKindOfClass:[NSNumber class]] || ![height isKindOfClass:[NSNumber class]]) {
        return NO;
    }
    CGRect decoded = CGRectMake(x.doubleValue, y.doubleValue, width.doubleValue, height.doubleValue);
    if (!isfinite(decoded.origin.x) || !isfinite(decoded.origin.y) ||
        !isfinite(decoded.size.width) || !isfinite(decoded.size.height) ||
        decoded.size.width <= 0.0 || decoded.size.height <= 0.0) {
        return NO;
    }
    *frame = decoded;
    return YES;
}

static BOOL GPTNullableStringsEqual(id expected, NSString *actual) {
    if (expected == nil || expected == [NSNull null]) {
        return actual == nil || actual.length == 0;
    }
    return [expected isKindOfClass:[NSString class]] && [actual isEqualToString:expected];
}

static BOOL GPTStringContainsNull(NSString *value) {
    unichar nullCharacter = 0;
    NSString *nullString = [NSString stringWithCharacters:&nullCharacter length:1];
    return [value rangeOfString:nullString].location != NSNotFound;
}

static BOOL GPTTargetIsFocused(pid_t processID, NSString *bundleID, AXUIElementRef window) {
    NSRunningApplication *frontmost = NSWorkspace.sharedWorkspace.frontmostApplication;
    if (frontmost == nil || frontmost.processIdentifier != processID ||
        ![frontmost.bundleIdentifier isEqualToString:bundleID]) {
        return NO;
    }
    AXUIElementRef application = AXUIElementCreateApplication(processID);
    if (application == NULL) {
        return NO;
    }
    CFTypeRef focusedValue = NULL;
    AXError focusedError = AXUIElementCopyAttributeValue(
        application, kAXFocusedWindowAttribute, &focusedValue);
    CFRelease(application);
    BOOL matches = focusedError == kAXErrorSuccess && focusedValue != NULL &&
                   CFGetTypeID(focusedValue) == AXUIElementGetTypeID() &&
                   CFEqual(focusedValue, window);
    if (focusedValue != NULL) {
        CFRelease(focusedValue);
    }
    return matches;
}

static AXUIElementRef GPTCopyAXElementAtIndexRecursive(
    AXUIElementRef element,
    NSUInteger depth,
    NSUInteger targetIndex,
    NSUInteger *nextIndex,
    NSMutableSet<NSValue *> *visited,
    BOOL *truncated) {
    if (depth > GPT_MAX_AX_DEPTH || visited.count >= 10000) {
        *truncated = YES;
        return NULL;
    }
    NSValue *identity = [NSValue valueWithPointer:(const void *)element];
    if ([visited containsObject:identity]) {
        return NULL;
    }
    [visited addObject:identity];

    NSString *role = GPTCopyAXString(element, kAXRoleAttribute);
    if (role.length > 0) {
        if (*nextIndex == targetIndex) {
            CFRetain(element);
            return element;
        }
        *nextIndex += 1;
    }

    CFTypeRef childrenValue = NULL;
    AXError childrenError =
        AXUIElementCopyAttributeValue(element, kAXChildrenAttribute, &childrenValue);
    AXUIElementRef found = NULL;
    if (childrenError == kAXErrorSuccess && childrenValue != NULL &&
        CFGetTypeID(childrenValue) == CFArrayGetTypeID()) {
        CFArrayRef children = (CFArrayRef)childrenValue;
        for (CFIndex index = 0; index < CFArrayGetCount(children) && found == NULL; index++) {
            found = GPTCopyAXElementAtIndexRecursive(
                (AXUIElementRef)CFArrayGetValueAtIndex(children, index),
                depth + 1,
                targetIndex,
                nextIndex,
                visited,
                truncated);
        }
    } else if ((childrenError != kAXErrorNoValue &&
                childrenError != kAXErrorAttributeUnsupported) ||
               childrenError == kAXErrorSuccess) {
        *truncated = YES;
    }
    if (childrenValue != NULL) {
        CFRelease(childrenValue);
    }
    return found;
}

static AXUIElementRef GPTCopyAXElementAtIndex(
    AXUIElementRef window,
    NSUInteger targetIndex,
    BOOL *truncated) {
    NSUInteger nextIndex = 0;
    NSMutableSet<NSValue *> *visited = [NSMutableSet set];
    return GPTCopyAXElementAtIndexRecursive(
        window, 0, targetIndex, &nextIndex, visited, truncated);
}

static BOOL GPTElementMatchesAttestation(
    AXUIElementRef element,
    NSDictionary *expected,
    NSString *requiredAction) {
    NSString *role = GPTCopyAXString(element, kAXRoleAttribute);
    NSString *subrole = GPTCopyAXString(element, kAXSubroleAttribute);
    NSString *label = GPTAXLabel(element);
    NSString *value = GPTCopyAXScalarDescription(element, kAXValueAttribute);
    NSString *expectedRole = expected[@"role"];
    NSString *expectedSensitivity = expected[@"sensitivity"];
    NSNumber *expectedEnabled = expected[@"enabled"];
    if (![expectedRole isKindOfClass:[NSString class]] ||
        ![expectedSensitivity isEqualToString:@"none"] ||
        ![expectedEnabled isKindOfClass:[NSNumber class]] || !expectedEnabled.boolValue ||
        ![role isEqualToString:expectedRole] ||
        !GPTNullableStringsEqual(expected[@"subrole"], subrole) ||
        !GPTNullableStringsEqual(expected[@"label"], label) ||
        !GPTNullableStringsEqual(expected[@"value"], value)) {
        return NO;
    }
    BOOL secure = [role localizedCaseInsensitiveContainsString:@"secure"] ||
                  [subrole localizedCaseInsensitiveContainsString:@"secure"];
    NSNumber *enabled = GPTCopyAXBool(element, kAXEnabledAttribute);
    if (secure || (enabled != nil && !enabled.boolValue)) {
        return NO;
    }
    id expectedFrameValue = expected[@"frame"];
    CGRect currentFrame = CGRectZero;
    if (expectedFrameValue == nil || expectedFrameValue == [NSNull null]) {
        if (GPTCopyAXFrame(element, &currentFrame)) {
            return NO;
        }
    } else {
        CGRect expectedFrame = CGRectZero;
        if (!GPTReadFrame(expectedFrameValue, &expectedFrame) ||
            !GPTCopyAXFrame(element, &currentFrame) ||
            !GPTFrameMatches(expectedFrame, currentFrame)) {
            return NO;
        }
    }
    return [GPTAXActions(element, role) containsObject:requiredAction];
}

static AXError GPTPerformNamedAction(AXUIElementRef element, CFStringRef preferred) {
    CFArrayRef actionNames = NULL;
    AXError listError = AXUIElementCopyActionNames(element, &actionNames);
    if (listError != kAXErrorSuccess || actionNames == NULL) {
        if (actionNames != NULL) {
            CFRelease(actionNames);
        }
        return listError == kAXErrorSuccess ? kAXErrorActionUnsupported : listError;
    }
    NSArray *actions = (__bridge NSArray *)actionNames;
    CFStringRef selected = NULL;
    if ([actions containsObject:(__bridge NSString *)preferred]) {
        selected = preferred;
    } else if ([actions containsObject:(__bridge NSString *)kAXPressAction]) {
        selected = kAXPressAction;
    } else if ([actions containsObject:(__bridge NSString *)kAXConfirmAction]) {
        selected = kAXConfirmAction;
    } else if ([actions containsObject:(__bridge NSString *)kAXShowMenuAction]) {
        selected = kAXShowMenuAction;
    }
    AXError result = selected == NULL ? kAXErrorActionUnsupported
                                      : AXUIElementPerformAction(element, selected);
    CFRelease(actionNames);
    return result;
}

static GPTMacNativeResult GPTActionResult(NSString *summary, id postcondition) {
    id normalizedPostcondition = postcondition == nil || postcondition == [NSNull null]
        ? [NSNull null]
        : @((BOOL)[postcondition boolValue]);
    return GPTJSONResult(
        @{
            @"summary" : summary,
            @"expectedPostconditionMet" : normalizedPostcondition,
        },
        nil);
}

static GPTMacNativeResult GPTActImpl(
    const uint8_t *requestBytes,
    size_t requestLength,
    const void *cancellation) {
    @autoreleasepool {
        if (gpt_macos_cancellation_is_signalled(cancellation)) {
            return GPTErrorResult(
                GPT_MAC_INTERRUPTED, @"macOS action was cancelled before native preflight");
        }
        if (requestBytes == NULL || requestLength == 0 || requestLength > 64 * 1024) {
            return GPTErrorResult(GPT_MAC_INVALID_REQUEST, @"invalid macOS action request size");
        }
        if (!gpt_macos_observation_supported()) {
            return GPTErrorResult(GPT_MAC_UNSUPPORTED, @"macOS 14 or later is required");
        }
        if (!CGPreflightScreenCaptureAccess() || !AXIsProcessTrusted()) {
            return GPTErrorResult(
                GPT_MAC_PERMISSION_REQUIRED,
                @"Screen Recording and Accessibility permissions are required");
        }
        NSData *requestData = [NSData dataWithBytes:requestBytes length:requestLength];
        NSError *jsonError = nil;
        id decoded = [NSJSONSerialization JSONObjectWithData:requestData options:0 error:&jsonError];
        if (![decoded isKindOfClass:[NSDictionary class]]) {
            return GPTErrorResult(GPT_MAC_INVALID_REQUEST, @"malformed macOS action request");
        }
        NSDictionary *request = decoded;
        NSNumber *windowNumber = request[@"windowId"];
        NSNumber *processNumber = request[@"processId"];
        NSString *bundleID = request[@"bundleId"];
        NSDictionary *action = request[@"action"];
        NSString *kind = [action isKindOfClass:[NSDictionary class]] ? action[@"kind"] : nil;
        NSString *executionMode = request[@"executionMode"];
        BOOL measuredBackground =
            [executionMode isKindOfClass:[NSString class]] &&
            [executionMode isEqualToString:@"measured_background"];
        CGRect expectedFrame = CGRectZero;
        if (![windowNumber isKindOfClass:[NSNumber class]] || windowNumber.unsignedIntValue == 0 ||
            ![processNumber isKindOfClass:[NSNumber class]] || processNumber.intValue <= 0 ||
            ![bundleID isKindOfClass:[NSString class]] || bundleID.length == 0 ||
            bundleID.length > 256 || GPTDeniedBundle(bundleID) ||
            ![action isKindOfClass:[NSDictionary class]] ||
            ![kind isKindOfClass:[NSString class]] ||
            ![executionMode isKindOfClass:[NSString class]] ||
            (![executionMode isEqualToString:@"foreground_required"] && !measuredBackground) ||
            !GPTReadFrame(request[@"expectedFrame"], &expectedFrame)) {
            return GPTErrorResult(GPT_MAC_INVALID_REQUEST, @"invalid macOS action binding");
        }
        NSSet<NSString *> *allowedKinds = [NSSet setWithArray:@[
            @"activate", @"invoke", @"set_value", @"select", @"scroll",
        ]];
        if (![allowedKinds containsObject:kind]) {
            return GPTErrorResult(GPT_MAC_FORBIDDEN_ACTION, @"unsupported macOS semantic action");
        }
        if (measuredBackground && ![kind isEqualToString:@"set_value"]) {
            return GPTErrorResult(
                GPT_MAC_FORBIDDEN_ACTION,
                @"this measured background backend supports visible text entry only");
        }

        if (@available(macOS 14.0, *)) {
            pid_t processID = processNumber.intValue;
            uint32_t windowID = windowNumber.unsignedIntValue;
            SCShareableContent *content = GPTShareableContent();
            if (gpt_macos_cancellation_is_signalled(cancellation)) {
                return GPTErrorResult(
                    GPT_MAC_INTERRUPTED, @"macOS action was cancelled during native preflight");
            }
            SCWindow *window = GPTFindWindow(content, windowID, processID, bundleID);
            if (window == nil) {
                return GPTErrorResult(GPT_MAC_TARGET_CLOSED, @"selected macOS window closed");
            }
            if (!window.isOnScreen && !window.isActive) {
                return GPTErrorResult(
                    GPT_MAC_TARGET_CLOSED, @"selected macOS window is minimized or hidden");
            }
            if (!GPTFrameMatches(expectedFrame, window.frame)) {
                return GPTErrorResult(
                    GPT_MAC_TARGET_CHANGED, @"selected macOS window geometry changed");
            }
            AXUIElementRef axWindow = GPTCopyMatchingAXWindow(processID, window.frame);
            if (axWindow == NULL) {
                return GPTErrorResult(
                    GPT_MAC_TARGET_CHANGED,
                    @"selected macOS window has no exact Accessibility match");
            }
            if (gpt_macos_cancellation_is_signalled(cancellation)) {
                CFRelease(axWindow);
                return GPTErrorResult(
                    GPT_MAC_INTERRUPTED, @"macOS action was cancelled before Accessibility dispatch");
            }

            if ([kind isEqualToString:@"activate"]) {
                if (measuredBackground) {
                    CFRelease(axWindow);
                    return GPTErrorResult(
                        GPT_MAC_FORBIDDEN_ACTION,
                        @"measured background dispatch cannot activate a target");
                }
                if (request[@"elementIndex"] != [NSNull null] ||
                    request[@"expectedElement"] != [NSNull null]) {
                    CFRelease(axWindow);
                    return GPTErrorResult(
                        GPT_MAC_INVALID_REQUEST, @"activation must not carry an element");
                }
                if (gpt_macos_cancellation_is_signalled(cancellation)) {
                    CFRelease(axWindow);
                    return GPTErrorResult(
                        GPT_MAC_INTERRUPTED, @"macOS activation was cancelled before dispatch");
                }
                NSRunningApplication *application =
                    [NSRunningApplication runningApplicationWithProcessIdentifier:processID];
                BOOL requested = application != nil &&
                    [application activateWithOptions:NSApplicationActivateIgnoringOtherApps];
                BOOL focused = NO;
                for (NSUInteger attempt = 0; requested && attempt < 40; attempt++) {
                    if (gpt_macos_cancellation_is_signalled(cancellation)) {
                        CFRelease(axWindow);
                        return GPTErrorResult(
                            GPT_MAC_INTERRUPTED,
                            @"macOS activation completion lost to local takeover");
                    }
                    if (GPTTargetIsFocused(processID, bundleID, axWindow)) {
                        focused = YES;
                        break;
                    }
                    [NSThread sleepForTimeInterval:0.025];
                }
                if (gpt_macos_cancellation_is_signalled(cancellation)) {
                    CFRelease(axWindow);
                    return GPTErrorResult(
                        GPT_MAC_INTERRUPTED,
                        @"macOS activation completion lost to local takeover");
                }
                CFRelease(axWindow);
                return focused
                    ? GPTActionResult(@"Activated the authorized macOS target", @YES)
                    : GPTErrorResult(
                          GPT_MAC_TARGET_CHANGED,
                          @"authorized macOS target did not become focused");
            }

            NSNumber *elementIndex = request[@"elementIndex"];
            NSDictionary *expectedElement = request[@"expectedElement"];
            NSString *requiredAction = [expectedElement isKindOfClass:[NSDictionary class]]
                ? expectedElement[@"requiredAction"]
                : nil;
            BOOL targetFocused = GPTTargetIsFocused(processID, bundleID, axWindow);
            if (![elementIndex isKindOfClass:[NSNumber class]] ||
                elementIndex.unsignedIntegerValue >= 10000 ||
                ![expectedElement isKindOfClass:[NSDictionary class]] ||
                ![requiredAction isKindOfClass:[NSString class]] ||
                ![requiredAction isEqualToString:kind] ||
                (measuredBackground ? targetFocused : !targetFocused)) {
                CFRelease(axWindow);
                return GPTErrorResult(
                    GPT_MAC_TARGET_CHANGED,
                    measuredBackground
                        ? @"measured background target became foreground before dispatch"
                        : @"authorized macOS target lost exact focus before dispatch");
            }
            BOOL traversalTruncated = NO;
            AXUIElementRef element = GPTCopyAXElementAtIndex(
                axWindow, elementIndex.unsignedIntegerValue, &traversalTruncated);
            if (element == NULL || traversalTruncated ||
                !GPTElementMatchesAttestation(element, expectedElement, requiredAction)) {
                if (element != NULL) {
                    CFRelease(element);
                }
                CFRelease(axWindow);
                return GPTErrorResult(
                    GPT_MAC_TARGET_CHANGED,
                    @"macOS element changed since the approved observation");
            }
            targetFocused = GPTTargetIsFocused(processID, bundleID, axWindow);
            if (measuredBackground ? targetFocused : !targetFocused) {
                CFRelease(element);
                CFRelease(axWindow);
                return GPTErrorResult(
                    GPT_MAC_TARGET_CHANGED,
                    measuredBackground
                        ? @"measured background target became foreground at dispatch boundary"
                        : @"authorized macOS target lost focus at dispatch boundary");
            }
            GPTMacUserInteractionState interactionBefore = {0};
            if (measuredBackground) {
                interactionBefore = GPTCaptureUserInteractionState();
                if (!interactionBefore.valid ||
                    interactionBefore.frontmost_process_id == processID) {
                    CFRelease(element);
                    CFRelease(axWindow);
                    return GPTErrorResult(
                        GPT_MAC_FORBIDDEN_ACTION,
                        @"foreground app/window/pointer state could not prove a background boundary");
                }
            }
            if (gpt_macos_cancellation_is_signalled(cancellation)) {
                CFRelease(element);
                CFRelease(axWindow);
                return GPTErrorResult(
                    GPT_MAC_INTERRUPTED, @"macOS semantic action was cancelled before dispatch");
            }

            AXError actionError = kAXErrorActionUnsupported;
            id postcondition = [NSNull null];
            NSString *summary = @"Completed a semantic macOS action";
            if ([kind isEqualToString:@"set_value"]) {
                NSString *text = action[@"text"];
                if (![text isKindOfClass:[NSString class]] ||
                    [text lengthOfBytesUsingEncoding:NSUTF8StringEncoding] > 4096 ||
                    GPTStringContainsNull(text)) {
                    CFRelease(element);
                    CFRelease(axWindow);
                    return GPTErrorResult(
                        GPT_MAC_INVALID_REQUEST, @"invalid macOS text entry payload");
                }
                Boolean settable = false;
                AXError settableError = AXUIElementIsAttributeSettable(
                    element, kAXValueAttribute, &settable);
                actionError = settableError == kAXErrorSuccess && settable
                    ? AXUIElementSetAttributeValue(
                          element, kAXValueAttribute, (__bridge CFTypeRef)text)
                    : kAXErrorAttributeUnsupported;
                NSString *updated = actionError == kAXErrorSuccess
                    ? GPTCopyAXScalarDescription(element, kAXValueAttribute)
                    : nil;
                postcondition = @(actionError == kAXErrorSuccess &&
                                  [updated isEqualToString:text]);
                summary = @"Set visible text on the authorized macOS element";
            } else if ([kind isEqualToString:@"invoke"]) {
                actionError = GPTPerformNamedAction(element, kAXPressAction);
                summary = @"Invoked the authorized macOS element";
            } else if ([kind isEqualToString:@"select"]) {
                Boolean selectedSettable = false;
                AXError selectedError = AXUIElementIsAttributeSettable(
                    element, kAXSelectedAttribute, &selectedSettable);
                if (selectedError == kAXErrorSuccess && selectedSettable) {
                    actionError = AXUIElementSetAttributeValue(
                        element, kAXSelectedAttribute, kCFBooleanTrue);
                    NSNumber *selected = actionError == kAXErrorSuccess
                        ? GPTCopyAXBool(element, kAXSelectedAttribute)
                        : nil;
                    postcondition = @((BOOL)selected.boolValue);
                } else {
                    actionError = GPTPerformNamedAction(element, kAXPressAction);
                }
                summary = @"Selected the authorized macOS element";
            } else if ([kind isEqualToString:@"scroll"]) {
                NSNumber *deltaX = action[@"deltaX"];
                NSNumber *deltaY = action[@"deltaY"];
                if (![deltaX isKindOfClass:[NSNumber class]] ||
                    ![deltaY isKindOfClass:[NSNumber class]] ||
                    (deltaX.intValue == 0 && deltaY.intValue == 0) ||
                    llabs(deltaX.longLongValue) > 10000 ||
                    llabs(deltaY.longLongValue) > 10000) {
                    CFRelease(element);
                    CFRelease(axWindow);
                    return GPTErrorResult(
                        GPT_MAC_INVALID_REQUEST, @"invalid semantic scroll request");
                }
                actionError = AXUIElementPerformAction(element, CFSTR("AXScrollToVisible"));
                summary = @"Scrolled the authorized macOS element into view";
            }

            BOOL cancellationAfterDispatch =
                gpt_macos_cancellation_is_signalled(cancellation);
            CFRelease(element);
            if (cancellationAfterDispatch) {
                CFRelease(axWindow);
                return GPTErrorResult(
                    GPT_MAC_INTERRUPTED,
                    @"macOS semantic action completion lost to local takeover");
            }
            if (measuredBackground) {
                GPTMacUserInteractionState interactionAfter =
                    GPTCaptureUserInteractionState();
                if (!GPTUserInteractionStateEqual(interactionBefore, interactionAfter) ||
                    GPTTargetIsFocused(processID, bundleID, axWindow)) {
                    CFRelease(axWindow);
                    return GPTErrorResult(
                        GPT_MAC_UNCERTAIN_OUTCOME,
                        @"background action changed foreground app, active window, or physical pointer");
                }
            }
            if (actionError != kAXErrorSuccess) {
                CFRelease(axWindow);
                return GPTErrorResult(
                    GPT_MAC_BACKEND_FAILURE, @"macOS rejected the semantic action");
            }
            if ([postcondition isKindOfClass:[NSNumber class]] &&
                ![postcondition boolValue]) {
                CFRelease(axWindow);
                return GPTErrorResult(
                    GPT_MAC_BACKEND_FAILURE,
                    @"macOS action postcondition could not be verified");
            }
            BOOL focusPreserved = GPTTargetIsFocused(processID, bundleID, axWindow);
            CFRelease(axWindow);
            SCWindow *afterWindow = GPTFindWindow(
                GPTShareableContent(), windowID, processID, bundleID);
            if ((measuredBackground ? focusPreserved : !focusPreserved) || afterWindow == nil ||
                !GPTFrameMatches(expectedFrame, afterWindow.frame)) {
                return GPTErrorResult(
                    GPT_MAC_TARGET_CHANGED,
                    measuredBackground
                        ? @"measured background target changed after action dispatch"
                        : @"authorized macOS target changed after action dispatch");
            }
            return GPTActionResult(summary, postcondition);
        }
        return GPTErrorResult(GPT_MAC_UNSUPPORTED, @"macOS 14 or later is required");
    }
}

GPTMacNativeResult gpt_macos_act(
    const uint8_t *requestBytes,
    size_t requestLength,
    const void *cancellation) {
    @try {
        return GPTActImpl(requestBytes, requestLength, cancellation);
    } @catch (NSException *exception) {
        (void)exception;
        return GPTErrorResult(
            GPT_MAC_BACKEND_FAILURE,
            @"macOS semantic action raised an Objective-C exception");
    }
}

GPTMacNativeResult gpt_macos_observe(
    uint32_t windowID,
    int32_t processID,
    const uint8_t *bundleBytes,
    size_t bundleLength,
    uint32_t maxNodes,
    uint32_t maxDimension,
    uint64_t maxPNGBytes) {
    @autoreleasepool {
        if (!gpt_macos_observation_supported()) {
            return GPTErrorResult(GPT_MAC_UNSUPPORTED, @"macOS 14 or later is required");
        }
        if (!CGPreflightScreenCaptureAccess() || !AXIsProcessTrusted()) {
            return GPTErrorResult(
                GPT_MAC_PERMISSION_REQUIRED,
                @"Screen Recording and Accessibility permissions are required");
        }
        NSString *bundleID = [[NSString alloc]
            initWithBytes:bundleBytes
                   length:bundleLength
                 encoding:NSUTF8StringEncoding];
        if (bundleID.length == 0 || GPTDeniedBundle(bundleID)) {
            return GPTErrorResult(GPT_MAC_SENSITIVE, @"macOS target is hard denied");
        }
        if (@available(macOS 14.0, *)) {
            SCShareableContent *beforeContent = GPTShareableContent();
            SCWindow *window = GPTFindWindow(beforeContent, windowID, processID, bundleID);
            if (window == nil) {
                return GPTErrorResult(GPT_MAC_TARGET_CLOSED, @"selected macOS window closed");
            }
            if (!window.isOnScreen && !window.isActive) {
                return GPTErrorResult(
                    GPT_MAC_TARGET_CLOSED, @"selected macOS window is minimized or hidden");
            }
            CGRect windowFrame = window.frame;
            AXUIElementRef axWindow = GPTCopyMatchingAXWindow(processID, windowFrame);
            if (axWindow == NULL) {
                return GPTErrorResult(
                    GPT_MAC_BACKEND_FAILURE,
                    @"selected macOS window has no matching Accessibility surface");
            }
            NSMutableArray<NSDictionary *> *nodes = [NSMutableArray array];
            NSMutableArray<NSValue *> *secureFrames = [NSMutableArray array];
            NSMutableSet<NSValue *> *visited = [NSMutableSet set];
            BOOL nodesTruncated = NO;
            GPTTraverseAX(
                axWindow,
                0,
                MIN(MAX((NSUInteger)maxNodes, (NSUInteger)1), (NSUInteger)10000),
                nodes,
                secureFrames,
                visited,
                &nodesTruncated);
            CFRelease(axWindow);

            uint32_t pixelWidth = 0;
            uint32_t pixelHeight = 0;
            int32_t captureStatus = GPT_MAC_BACKEND_FAILURE;
            NSData *png = GPTCaptureRedactedWindow(
                window,
                windowFrame,
                secureFrames,
                maxDimension,
                maxPNGBytes,
                &pixelWidth,
                &pixelHeight,
                &captureStatus);
            if (png == nil) {
                return GPTErrorResult(captureStatus, @"macOS screenshot capture failed");
            }

            SCShareableContent *afterContent = GPTShareableContent();
            SCWindow *afterWindow = GPTFindWindow(afterContent, windowID, processID, bundleID);
            if (afterWindow == nil) {
                return GPTErrorResult(
                    GPT_MAC_TARGET_CLOSED, @"selected macOS window closed during observation");
            }
            if (!GPTFrameMatches(windowFrame, afterWindow.frame)) {
                return GPTErrorResult(
                    GPT_MAC_TARGET_CHANGED, @"selected macOS window moved during observation");
            }

            NSDictionary *identity = @{
                @"windowId" : @(windowID),
                @"processId" : @(processID),
                @"bundleId" : bundleID,
            };
            NSDictionary *observation = @{
                @"identity" : identity,
                @"frame" : GPTFrameDictionary(windowFrame),
                @"pixelWidth" : @(pixelWidth),
                @"pixelHeight" : @(pixelHeight),
                @"privacyRedacted" : @YES,
                @"nodes" : nodes,
                @"nodesTruncated" : @((BOOL)nodesTruncated),
                @"sensitivity" : @"none",
            };
            return GPTJSONResult(observation, png);
        }
        return GPTErrorResult(GPT_MAC_UNSUPPORTED, @"macOS 14 or later is required");
    }
}
