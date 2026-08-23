#import <AppKit/AppKit.h>
#import <ApplicationServices/ApplicationServices.h>
#import <CoreGraphics/CoreGraphics.h>
#import <Foundation/Foundation.h>
#import <ScreenCaptureKit/ScreenCaptureKit.h>
#import <Security/Security.h>

#include <stdbool.h>
#include <dlfcn.h>
#include <stdatomic.h>
#include <stdint.h>
#include <math.h>
#include <stdlib.h>
#include <string.h>

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
    CGEventRef event = CGEventCreate(NULL);
    if (event == NULL) {
        return state;
    }
    state.pointer_location = CGEventGetLocation(event);
    CFRelease(event);
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
