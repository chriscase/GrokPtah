#import <AppKit/AppKit.h>
#import <ApplicationServices/ApplicationServices.h>
#import <CoreGraphics/CoreGraphics.h>
#import <Foundation/Foundation.h>
#import <ScreenCaptureKit/ScreenCaptureKit.h>

#include <stdbool.h>
#include <dlfcn.h>
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

enum {
    GPT_MAC_OK = 0,
    GPT_MAC_UNSUPPORTED = 1,
    GPT_MAC_PERMISSION_REQUIRED = 2,
    GPT_MAC_TARGET_CLOSED = 3,
    GPT_MAC_TARGET_CHANGED = 4,
    GPT_MAC_SENSITIVE = 5,
    GPT_MAC_LIMIT_REACHED = 6,
    GPT_MAC_BACKEND_FAILURE = 7,
};

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
    return application != nil && !GPTDeniedBundle(application.bundleIdentifier) &&
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
        @"minimized" : @(!onScreen && !active),
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
        result = @(CFBooleanGetValue((CFBooleanRef)value));
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
                @"nodesTruncated" : @(nodesTruncated),
                @"sensitivity" : @"none",
            };
            return GPTJSONResult(observation, png);
        }
        return GPTErrorResult(GPT_MAC_UNSUPPORTED, @"macOS 14 or later is required");
    }
}
