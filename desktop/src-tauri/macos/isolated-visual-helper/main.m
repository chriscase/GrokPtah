#import <Foundation/Foundation.h>
#import <Security/Security.h>
#import <Virtualization/Virtualization.h>

#include "../isolated-visual-guest/protocol.h"

#include <errno.h>
#include <crt_externs.h>
#include <fcntl.h>
#include <math.h>
#include <poll.h>
#include <signal.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

static const int GPT_GUEST_IMAGE_FD = 3;
static const int GPT_CONFIGURATION_FD = 4;
static const int GPT_CONTROL_FD = 5;
static const int GPT_EVENT_FD = 6;
static const size_t GPT_MAX_CONFIGURATION_BYTES = 1024 * 1024;
static const uint8_t GPT_CONTROL_START = 1;
static const uint8_t GPT_CONTROL_STOP = 2;
static const int GPT_GUEST_HANDSHAKE_TIMEOUT_MS = 30000;
static const int GPT_GUEST_SHUTDOWN_TIMEOUT_MS = 5000;
static const uint32_t GPT_EVENT_MAGIC = 0x47505449;
static const uint16_t GPT_EVENT_VERSION = 1;
static NSString *const GPT_KERNEL_COMMAND_LINE =
    @"panic=-1 reboot=t init=/init grokptah.isolated_visual=1";
static char *GPT_EMPTY_ENVIRONMENT[] = {NULL};

typedef NS_ENUM(uint16_t, GPTIsolatedHelperEventCode) {
    GPTIsolatedHelperEventPrepared = 1,
    GPTIsolatedHelperEventRunning = 2,
    GPTIsolatedHelperEventStopped = 3,
    GPTIsolatedHelperEventFailure = 4,
};

typedef NS_ENUM(uint32_t, GPTIsolatedHelperFailure) {
    GPTIsolatedHelperFailureInvalidInvocation = 1,
    GPTIsolatedHelperFailureInvalidDescriptor = 2,
    GPTIsolatedHelperFailureInvalidConfiguration = 3,
    GPTIsolatedHelperFailureStartNotAuthorized = 4,
    GPTIsolatedHelperFailureVirtualizationUnavailable = 5,
    GPTIsolatedHelperFailureConfigurationRejected = 6,
    GPTIsolatedHelperFailureStartFailed = 7,
    GPTIsolatedHelperFailureControlLost = 8,
    GPTIsolatedHelperFailureStopFailed = 9,
    GPTIsolatedHelperFailureGuestStopped = 10,
    GPTIsolatedHelperFailureGuestProtocol = 11,
};

typedef struct __attribute__((packed)) {
    uint32_t magic;
    uint16_t version;
    uint16_t code;
    uint32_t detail;
    uint32_t reserved;
} GPTIsolatedHelperEvent;

typedef struct {
    NSUInteger cpuCount;
    uint64_t memoryBytes;
    NSUInteger displayWidth;
    NSUInteger displayHeight;
    NSUInteger durationSeconds;
} GPTIsolatedHelperConfiguration;

static volatile sig_atomic_t GPTStopRequested = 0;

static void GPTSignalHandler(int signalNumber) {
    (void)signalNumber;
    GPTStopRequested = 1;
}

static BOOL GPTWriteExact(int descriptor, const void *bytes, size_t length) {
    const uint8_t *cursor = bytes;
    size_t remaining = length;
    while (remaining > 0) {
        ssize_t written = write(descriptor, cursor, remaining);
        if (written < 0 && errno == EINTR) {
            continue;
        }
        if (written <= 0) {
            return NO;
        }
        cursor += written;
        remaining -= (size_t)written;
    }
    return YES;
}

static BOOL GPTWriteEvent(GPTIsolatedHelperEventCode code, uint32_t detail) {
    GPTIsolatedHelperEvent event = {
        .magic = CFSwapInt32HostToBig(GPT_EVENT_MAGIC),
        .version = CFSwapInt16HostToBig(GPT_EVENT_VERSION),
        .code = CFSwapInt16HostToBig(code),
        .detail = CFSwapInt32HostToBig(detail),
        .reserved = 0,
    };
    return GPTWriteExact(GPT_EVENT_FD, &event, sizeof(event));
}

static BOOL GPTDescriptorHasAccess(int descriptor, int accessMode) {
    int flags = fcntl(descriptor, F_GETFL);
    return flags >= 0 && (flags & O_ACCMODE) == accessMode;
}

static BOOL GPTValidateDescriptors(void) {
    struct stat guest = {0};
    struct stat configuration = {0};
    struct stat control = {0};
    struct stat events = {0};
    if (!GPTDescriptorHasAccess(GPT_GUEST_IMAGE_FD, O_RDONLY) ||
        !GPTDescriptorHasAccess(GPT_CONFIGURATION_FD, O_RDONLY) ||
        !GPTDescriptorHasAccess(GPT_CONTROL_FD, O_RDONLY) ||
        !GPTDescriptorHasAccess(GPT_EVENT_FD, O_WRONLY) ||
        fstat(GPT_GUEST_IMAGE_FD, &guest) != 0 ||
        fstat(GPT_CONFIGURATION_FD, &configuration) != 0 ||
        fstat(GPT_CONTROL_FD, &control) != 0 || fstat(GPT_EVENT_FD, &events) != 0) {
        return NO;
    }
    if (!S_ISREG(guest.st_mode) || !S_ISREG(configuration.st_mode) || guest.st_size <= 0 ||
        configuration.st_size <= 0 ||
        (uint64_t)guest.st_size > 32ULL * 1024 * 1024 * 1024 ||
        (uint64_t)configuration.st_size > GPT_MAX_CONFIGURATION_BYTES ||
        (guest.st_mode & 0133) != 0 || (configuration.st_mode & 0133) != 0 ||
        lseek(GPT_GUEST_IMAGE_FD, 0, SEEK_SET) != 0 ||
        lseek(GPT_CONFIGURATION_FD, 0, SEEK_SET) != 0) {
        return NO;
    }
    BOOL controlIsPrivateChannel = S_ISFIFO(control.st_mode) || S_ISSOCK(control.st_mode);
    BOOL eventsIsPrivateChannel = S_ISFIFO(events.st_mode) || S_ISSOCK(events.st_mode);
    return controlIsPrivateChannel && eventsIsPrivateChannel;
}

static NSData *GPTReadBoundedConfiguration(void) {
    uint8_t *buffer = malloc(GPT_MAX_CONFIGURATION_BYTES + 1);
    if (buffer == NULL) {
        return nil;
    }
    size_t length = 0;
    while (length <= GPT_MAX_CONFIGURATION_BYTES) {
        ssize_t count = read(
            GPT_CONFIGURATION_FD,
            buffer + length,
            GPT_MAX_CONFIGURATION_BYTES + 1 - length);
        if (count < 0 && errno == EINTR) {
            continue;
        }
        if (count < 0) {
            free(buffer);
            return nil;
        }
        if (count == 0) {
            break;
        }
        length += (size_t)count;
    }
    if (length == 0 || length > GPT_MAX_CONFIGURATION_BYTES) {
        free(buffer);
        return nil;
    }
    return [NSData dataWithBytesNoCopy:buffer length:length freeWhenDone:YES];
}

static BOOL GPTDictionaryHasExactKeys(NSDictionary *dictionary, NSArray<NSString *> *keys) {
    if (![dictionary isKindOfClass:[NSDictionary class]] || dictionary.count != keys.count) {
        return NO;
    }
    for (NSString *key in keys) {
        if (dictionary[key] == nil) {
            return NO;
        }
    }
    return YES;
}

static BOOL GPTBooleanIs(NSDictionary *dictionary, NSString *key, BOOL expected) {
    id value = dictionary[key];
    return value != nil && CFGetTypeID((__bridge CFTypeRef)value) == CFBooleanGetTypeID() &&
           [value boolValue] == expected;
}

static BOOL GPTUnsignedInteger(
    NSDictionary *dictionary,
    NSString *key,
    uint64_t minimum,
    uint64_t maximum,
    uint64_t *result) {
    id value = dictionary[key];
    if (![value isKindOfClass:[NSNumber class]] ||
        CFGetTypeID((__bridge CFTypeRef)value) == CFBooleanGetTypeID()) {
        return NO;
    }
    double floating = [value doubleValue];
    if (!isfinite(floating) || floating < (double)minimum || floating > (double)maximum ||
        floor(floating) != floating) {
        return NO;
    }
    uint64_t integer = [value unsignedLongLongValue];
    if ((double)integer != floating) {
        return NO;
    }
    *result = integer;
    return YES;
}

static BOOL GPTParseConfiguration(
    NSData *data,
    GPTIsolatedHelperConfiguration *configuration) {
    NSError *error = nil;
    id object = [NSJSONSerialization JSONObjectWithData:data options:0 error:&error];
    if (error != nil || ![object isKindOfClass:[NSDictionary class]]) {
        return NO;
    }
    NSDictionary *root = object;
    if (!GPTDictionaryHasExactKeys(
            root,
            @[
                @"schemaVersion",
                @"guestProtocolVersion",
                @"kernelCommandLine",
                @"securityProfile",
                @"limits",
            ])) {
        return NO;
    }
    uint64_t schemaVersion = 0;
    uint64_t guestProtocolVersion = 0;
    if (!GPTUnsignedInteger(root, @"schemaVersion", 1, 1, &schemaVersion) ||
        !GPTUnsignedInteger(root, @"guestProtocolVersion", 1, 1, &guestProtocolVersion) ||
        ![root[@"kernelCommandLine"] isKindOfClass:[NSString class]] ||
        ![root[@"kernelCommandLine"] isEqualToString:GPT_KERNEL_COMMAND_LINE]) {
        return NO;
    }

    NSDictionary *security = root[@"securityProfile"];
    if (!GPTDictionaryHasExactKeys(
            security,
            @[
                @"networkDevices",
                @"hostClipboard",
                @"sharedDirectories",
                @"credentialForwarding",
                @"hostInputForwarding",
                @"usbPassthrough",
                @"camera",
                @"microphone",
            ])) {
        return NO;
    }
    uint64_t networkDevices = 0;
    if (!GPTUnsignedInteger(security, @"networkDevices", 0, 0, &networkDevices)) {
        return NO;
    }
    for (NSString *key in @[
             @"hostClipboard",
             @"sharedDirectories",
             @"credentialForwarding",
             @"hostInputForwarding",
             @"usbPassthrough",
             @"camera",
             @"microphone",
         ]) {
        if (!GPTBooleanIs(security, key, NO)) {
            return NO;
        }
    }

    NSDictionary *limits = root[@"limits"];
    if (!GPTDictionaryHasExactKeys(
            limits,
            @[
                @"virtualCpus",
                @"memoryMib",
                @"overlayBytes",
                @"displayWidth",
                @"displayHeight",
                @"framesPerSecond",
                @"encodedFrameBytes",
                @"durationSeconds",
                @"inputEvents",
                @"textEventBytes",
            ])) {
        return NO;
    }
    uint64_t virtualCpus = 0;
    uint64_t memoryMib = 0;
    uint64_t overlayBytes = 0;
    uint64_t displayWidth = 0;
    uint64_t displayHeight = 0;
    uint64_t framesPerSecond = 0;
    uint64_t encodedFrameBytes = 0;
    uint64_t durationSeconds = 0;
    uint64_t inputEvents = 0;
    uint64_t textEventBytes = 0;
    if (!GPTUnsignedInteger(limits, @"virtualCpus", 1, 2, &virtualCpus) ||
        !GPTUnsignedInteger(limits, @"memoryMib", 512, 4096, &memoryMib) ||
        !GPTUnsignedInteger(limits, @"overlayBytes", 1, 8ULL * 1024 * 1024 * 1024, &overlayBytes) ||
        !GPTUnsignedInteger(limits, @"displayWidth", 1, 1280, &displayWidth) ||
        !GPTUnsignedInteger(limits, @"displayHeight", 1, 800, &displayHeight) ||
        !GPTUnsignedInteger(limits, @"framesPerSecond", 1, 10, &framesPerSecond) ||
        !GPTUnsignedInteger(
            limits,
            @"encodedFrameBytes",
            1,
            16ULL * 1024 * 1024,
            &encodedFrameBytes) ||
        !GPTUnsignedInteger(limits, @"durationSeconds", 1, 30 * 60, &durationSeconds) ||
        !GPTUnsignedInteger(limits, @"inputEvents", 1, 256, &inputEvents) ||
        !GPTUnsignedInteger(limits, @"textEventBytes", 1, 4096, &textEventBytes)) {
        return NO;
    }
    configuration->cpuCount = (NSUInteger)virtualCpus;
    configuration->memoryBytes = memoryMib * 1024 * 1024;
    configuration->displayWidth = (NSUInteger)displayWidth;
    configuration->displayHeight = (NSUInteger)displayHeight;
    configuration->durationSeconds = (NSUInteger)durationSeconds;
    return YES;
}

static VZVirtualMachineConfiguration *GPTVirtualMachineConfiguration(
    GPTIsolatedHelperConfiguration configuration,
    NSError **error) API_AVAILABLE(macos(14.0)) {
    NSURL *kernelURL = [NSURL fileURLWithPath:@"/dev/fd/3" isDirectory:NO];
    VZLinuxBootLoader *bootLoader = [[VZLinuxBootLoader alloc] initWithKernelURL:kernelURL];
    bootLoader.commandLine = GPT_KERNEL_COMMAND_LINE;

    VZVirtioGraphicsScanoutConfiguration *scanout =
        [[VZVirtioGraphicsScanoutConfiguration alloc]
            initWithWidthInPixels:configuration.displayWidth
                   heightInPixels:configuration.displayHeight];
    VZVirtioGraphicsDeviceConfiguration *graphics =
        [[VZVirtioGraphicsDeviceConfiguration alloc] init];
    graphics.scanouts = @[ scanout ];

    VZVirtualMachineConfiguration *machine = [[VZVirtualMachineConfiguration alloc] init];
    machine.bootLoader = bootLoader;
    machine.CPUCount = configuration.cpuCount;
    machine.memorySize = configuration.memoryBytes;
    machine.graphicsDevices = @[ graphics ];
    machine.entropyDevices = @[ [[VZVirtioEntropyDeviceConfiguration alloc] init] ];
    machine.socketDevices = @[ [[VZVirtioSocketDeviceConfiguration alloc] init] ];
    machine.networkDevices = @[];
    machine.directorySharingDevices = @[];
    machine.audioDevices = @[];
    machine.storageDevices = @[];
    machine.keyboards = @[];
    machine.pointingDevices = @[];
    machine.serialPorts = @[];
    if (![machine validateWithError:error]) {
        return nil;
    }
    return machine;
}

@interface GPTVirtualMachineDelegate : NSObject <VZVirtualMachineDelegate>
@property(nonatomic) dispatch_semaphore_t stopped;
@property(nonatomic, strong, nullable) NSError *failure;
@property(atomic) BOOL didStop;
@end

@implementation GPTVirtualMachineDelegate
- (instancetype)init {
    self = [super init];
    if (self != nil) {
        _stopped = dispatch_semaphore_create(0);
    }
    return self;
}

- (void)guestDidStopVirtualMachine:(VZVirtualMachine *)virtualMachine {
    (void)virtualMachine;
    self.didStop = YES;
    dispatch_semaphore_signal(self.stopped);
}

- (void)virtualMachine:(VZVirtualMachine *)virtualMachine didStopWithError:(NSError *)error {
    (void)virtualMachine;
    self.failure = error;
    self.didStop = YES;
    dispatch_semaphore_signal(self.stopped);
}
@end

@interface GPTGuestSocketDelegate : NSObject <VZVirtioSocketListenerDelegate>
@property(nonatomic, strong, nullable) VZVirtioSocketConnection *connection;
@property(nonatomic) dispatch_semaphore_t connected;
@end

@implementation GPTGuestSocketDelegate
- (instancetype)init {
    self = [super init];
    if (self != nil) {
        _connected = dispatch_semaphore_create(0);
    }
    return self;
}

- (BOOL)listener:(VZVirtioSocketListener *)listener
    shouldAcceptNewConnection:(VZVirtioSocketConnection *)connection
             fromSocketDevice:(VZVirtioSocketDevice *)socketDevice {
    (void)listener;
    (void)socketDevice;
    if (self.connection != nil || connection.fileDescriptor < 0) {
        return NO;
    }
    self.connection = connection;
    dispatch_semaphore_signal(self.connected);
    return YES;
}
@end

static int GPTReadControlByte(uint8_t *command, int timeoutMilliseconds) {
    struct pollfd descriptor = {
        .fd = GPT_CONTROL_FD,
        .events = POLLIN | POLLHUP,
        .revents = 0,
    };
    int polled;
    do {
        polled = poll(&descriptor, 1, timeoutMilliseconds);
    } while (polled < 0 && errno == EINTR && !GPTStopRequested);
    if (polled <= 0) {
        return polled;
    }
    ssize_t count;
    do {
        count = read(GPT_CONTROL_FD, command, 1);
    } while (count < 0 && errno == EINTR && !GPTStopRequested);
    return count == 1 ? 1 : -1;
}

static uint64_t GPTMonotonicMilliseconds(void) {
    struct timespec now = {0};
    if (clock_gettime(CLOCK_MONOTONIC, &now) != 0) {
        return 0;
    }
    return (uint64_t)now.tv_sec * 1000ULL + (uint64_t)now.tv_nsec / 1000000ULL;
}

static BOOL GPTWriteSocketExact(
    VZVirtioSocketConnection *connection,
    const void *bytes,
    size_t length,
    int timeoutMilliseconds) {
    const uint8_t *cursor = bytes;
    size_t remaining = length;
    uint64_t deadline = GPTMonotonicMilliseconds() + (uint64_t)timeoutMilliseconds;
    while (remaining > 0 && connection.fileDescriptor >= 0) {
        uint64_t now = GPTMonotonicMilliseconds();
        int waitMilliseconds = now >= deadline ? 0 : (int)(deadline - now);
        struct pollfd descriptor = {
            .fd = connection.fileDescriptor,
            .events = POLLOUT | POLLERR | POLLHUP,
            .revents = 0,
        };
        int polled;
        do {
            polled = poll(&descriptor, 1, waitMilliseconds);
        } while (polled < 0 && errno == EINTR && !GPTStopRequested);
        if (polled <= 0 ||
            (descriptor.revents & POLLERR) != 0 ||
            ((descriptor.revents & POLLHUP) != 0 &&
             (descriptor.revents & POLLOUT) == 0)) {
            return NO;
        }
        ssize_t written = write(connection.fileDescriptor, cursor, remaining);
        if (written < 0 && errno == EINTR) {
            continue;
        }
        if (written <= 0) {
            return NO;
        }
        cursor += written;
        remaining -= (size_t)written;
    }
    return remaining == 0;
}

static BOOL GPTReadSocketExact(
    VZVirtioSocketConnection *connection,
    void *bytes,
    size_t length,
    int timeoutMilliseconds) {
    uint8_t *cursor = bytes;
    size_t remaining = length;
    uint64_t deadline = GPTMonotonicMilliseconds() + (uint64_t)timeoutMilliseconds;
    while (remaining > 0 && connection.fileDescriptor >= 0) {
        uint64_t now = GPTMonotonicMilliseconds();
        int waitMilliseconds = now >= deadline ? 0 : (int)(deadline - now);
        struct pollfd descriptor = {
            .fd = connection.fileDescriptor,
            .events = POLLIN | POLLERR | POLLHUP,
            .revents = 0,
        };
        int polled;
        do {
            polled = poll(&descriptor, 1, waitMilliseconds);
        } while (polled < 0 && errno == EINTR && !GPTStopRequested);
        if (polled <= 0 ||
            (descriptor.revents & POLLERR) != 0 ||
            ((descriptor.revents & POLLHUP) != 0 &&
             (descriptor.revents & POLLIN) == 0)) {
            return NO;
        }
        ssize_t count = read(connection.fileDescriptor, cursor, remaining);
        if (count < 0 && errno == EINTR) {
            continue;
        }
        if (count <= 0) {
            return NO;
        }
        cursor += count;
        remaining -= (size_t)count;
    }
    return remaining == 0;
}

static BOOL GPTGuestWaitForReady(
    GPTGuestSocketDelegate *socketDelegate,
    gpt_u8 challenge[GPT_GUEST_BOOTSTRAP_CHALLENGE_BYTES]) {
    if (dispatch_semaphore_wait(
            socketDelegate.connected,
            dispatch_time(
                DISPATCH_TIME_NOW,
                (int64_t)GPT_GUEST_HANDSHAKE_TIMEOUT_MS * NSEC_PER_MSEC)) != 0) {
        return NO;
    }
    VZVirtioSocketConnection *connection = socketDelegate.connection;
    if (connection == nil ||
        SecRandomCopyBytes(
            kSecRandomDefault,
            GPT_GUEST_BOOTSTRAP_CHALLENGE_BYTES,
            challenge) != errSecSuccess ||
        !GPTWriteSocketExact(
            connection,
            challenge,
            GPT_GUEST_BOOTSTRAP_CHALLENGE_BYTES,
            GPT_GUEST_HANDSHAKE_TIMEOUT_MS)) {
        return NO;
    }
    gpt_u8 frame[GPT_GUEST_BOOTSTRAP_FRAME_BYTES] = {0};
    return GPTReadSocketExact(
               connection,
               frame,
               sizeof(frame),
               GPT_GUEST_HANDSHAKE_TIMEOUT_MS) &&
           gpt_guest_bootstrap_frame_valid(
               challenge,
               GPT_GUEST_BOOTSTRAP_EVENT_READY,
               frame);
}

static BOOL GPTGuestRequestShutdown(
    GPTGuestSocketDelegate *socketDelegate,
    const gpt_u8 challenge[GPT_GUEST_BOOTSTRAP_CHALLENGE_BYTES]) {
    VZVirtioSocketConnection *connection = socketDelegate.connection;
    if (connection == nil) {
        return NO;
    }
    const gpt_u8 stop = GPT_GUEST_BOOTSTRAP_STOP;
    if (!GPTWriteSocketExact(
            connection,
            &stop,
            sizeof(stop),
            GPT_GUEST_SHUTDOWN_TIMEOUT_MS)) {
        return NO;
    }
    gpt_u8 frame[GPT_GUEST_BOOTSTRAP_FRAME_BYTES] = {0};
    return GPTReadSocketExact(
               connection,
               frame,
               sizeof(frame),
               GPT_GUEST_SHUTDOWN_TIMEOUT_MS) &&
           gpt_guest_bootstrap_frame_valid(
               challenge,
               GPT_GUEST_BOOTSTRAP_EVENT_SHUTDOWN_ACK,
               frame);
}

static BOOL GPTStopVirtualMachine(
    VZVirtualMachine *virtualMachine,
    GPTVirtualMachineDelegate *delegate,
    dispatch_queue_t queue) API_AVAILABLE(macos(14.0)) {
    if (delegate.didStop) {
        return delegate.failure == nil;
    }
    dispatch_semaphore_t gracefulRequest = dispatch_semaphore_create(0);
    __block BOOL requested = NO;
    dispatch_async(queue, ^{
        NSError *error = nil;
        if (virtualMachine.canRequestStop) {
            requested = [virtualMachine requestStopWithError:&error];
        }
        dispatch_semaphore_signal(gracefulRequest);
    });
    long gracefulWait = dispatch_semaphore_wait(
        gracefulRequest,
        dispatch_time(DISPATCH_TIME_NOW, 2 * NSEC_PER_SEC));
    if (gracefulWait == 0 && requested &&
        dispatch_semaphore_wait(
            delegate.stopped,
            dispatch_time(DISPATCH_TIME_NOW, 2 * NSEC_PER_SEC)) == 0) {
        return delegate.failure == nil;
    }

    dispatch_semaphore_t forcedStop = dispatch_semaphore_create(0);
    __block NSError *stopError = nil;
    __block BOOL terminal = NO;
    dispatch_async(queue, ^{
        if (virtualMachine.canStop) {
            [virtualMachine stopWithCompletionHandler:^(NSError *error) {
                stopError = error;
                terminal = error == nil;
                dispatch_semaphore_signal(forcedStop);
            }];
        } else {
            terminal = virtualMachine.state == VZVirtualMachineStateStopped;
            dispatch_semaphore_signal(forcedStop);
        }
    });
    return dispatch_semaphore_wait(
               forcedStop,
               dispatch_time(DISPATCH_TIME_NOW, 10 * NSEC_PER_SEC)) == 0 &&
           stopError == nil && terminal;
}

int main(int argc, const char *argv[]) {
    (void)argv;
    umask(077);
    signal(SIGPIPE, SIG_IGN);
    signal(SIGINT, GPTSignalHandler);
    signal(SIGTERM, GPTSignalHandler);
    if (argc != 1) {
        return GPTIsolatedHelperFailureInvalidInvocation;
    }
    *_NSGetEnviron() = GPT_EMPTY_ENVIRONMENT;

    @autoreleasepool {
        if (@available(macOS 14.0, *)) {
            if (!GPTValidateDescriptors()) {
                GPTWriteEvent(
                    GPTIsolatedHelperEventFailure,
                    GPTIsolatedHelperFailureInvalidDescriptor);
                return GPTIsolatedHelperFailureInvalidDescriptor;
            }
            NSData *configurationData = GPTReadBoundedConfiguration();
            GPTIsolatedHelperConfiguration configuration = {0};
            if (configurationData == nil ||
                !GPTParseConfiguration(configurationData, &configuration)) {
                GPTWriteEvent(
                    GPTIsolatedHelperEventFailure,
                    GPTIsolatedHelperFailureInvalidConfiguration);
                return GPTIsolatedHelperFailureInvalidConfiguration;
            }
            if (!GPTWriteEvent(GPTIsolatedHelperEventPrepared, 0)) {
                return GPTIsolatedHelperFailureControlLost;
            }
            uint8_t command = 0;
            if (GPTReadControlByte(&command, 30000) != 1 || command != GPT_CONTROL_START ||
                GPTStopRequested) {
                GPTWriteEvent(
                    GPTIsolatedHelperEventFailure,
                    GPTIsolatedHelperFailureStartNotAuthorized);
                return GPTIsolatedHelperFailureStartNotAuthorized;
            }
            if (!VZVirtualMachine.supported) {
                GPTWriteEvent(
                    GPTIsolatedHelperEventFailure,
                    GPTIsolatedHelperFailureVirtualizationUnavailable);
                return GPTIsolatedHelperFailureVirtualizationUnavailable;
            }
            NSError *configurationError = nil;
            VZVirtualMachineConfiguration *machineConfiguration =
                GPTVirtualMachineConfiguration(configuration, &configurationError);
            if (machineConfiguration == nil) {
                GPTWriteEvent(
                    GPTIsolatedHelperEventFailure,
                    GPTIsolatedHelperFailureConfigurationRejected);
                return GPTIsolatedHelperFailureConfigurationRejected;
            }

            dispatch_queue_t queue = dispatch_queue_create(
                "com.chriscase.grokptah.isolated-visual-helper.vm",
                DISPATCH_QUEUE_SERIAL);
            VZVirtualMachine *virtualMachine = [[VZVirtualMachine alloc]
                initWithConfiguration:machineConfiguration
                                  queue:queue];
            GPTGuestSocketDelegate *guestSocketDelegate =
                [[GPTGuestSocketDelegate alloc] init];
            VZVirtioSocketListener *guestSocketListener =
                [[VZVirtioSocketListener alloc] init];
            guestSocketListener.delegate = guestSocketDelegate;
            VZSocketDevice *socketDevice = virtualMachine.socketDevices.firstObject;
            if (![socketDevice isKindOfClass:[VZVirtioSocketDevice class]]) {
                GPTWriteEvent(
                    GPTIsolatedHelperEventFailure,
                    GPTIsolatedHelperFailureConfigurationRejected);
                return GPTIsolatedHelperFailureConfigurationRejected;
            }
            [(VZVirtioSocketDevice *)socketDevice
                setSocketListener:guestSocketListener
                           forPort:GPT_GUEST_BOOTSTRAP_PORT];
            GPTVirtualMachineDelegate *delegate = [[GPTVirtualMachineDelegate alloc] init];
            virtualMachine.delegate = delegate;
            dispatch_semaphore_t started = dispatch_semaphore_create(0);
            __block NSError *startError = nil;
            dispatch_async(queue, ^{
                [virtualMachine startWithCompletionHandler:^(NSError *error) {
                    startError = error;
                    dispatch_semaphore_signal(started);
                }];
            });
            if (dispatch_semaphore_wait(
                    started,
                    dispatch_time(DISPATCH_TIME_NOW, 30 * NSEC_PER_SEC)) != 0 ||
                startError != nil) {
                GPTWriteEvent(
                    GPTIsolatedHelperEventFailure,
                    GPTIsolatedHelperFailureStartFailed);
                return GPTIsolatedHelperFailureStartFailed;
            }
            gpt_u8 challenge[GPT_GUEST_BOOTSTRAP_CHALLENGE_BYTES] = {0};
            if (!GPTGuestWaitForReady(guestSocketDelegate, challenge)) {
                GPTStopVirtualMachine(virtualMachine, delegate, queue);
                GPTWriteEvent(
                    GPTIsolatedHelperEventFailure,
                    GPTIsolatedHelperFailureGuestProtocol);
                return GPTIsolatedHelperFailureGuestProtocol;
            }
            if (!GPTWriteEvent(GPTIsolatedHelperEventRunning, 0)) {
                GPTStopRequested = 1;
            }

            NSUInteger elapsedSeconds = 0;
            BOOL controlLost = NO;
            BOOL guestStopped = NO;
            BOOL expectedGuestStop = NO;
            BOOL guestShutdownAcknowledged = NO;
            while (!GPTStopRequested && elapsedSeconds < configuration.durationSeconds) {
                if (delegate.didStop) {
                    guestStopped = YES;
                    break;
                }
                command = 0;
                int result = GPTReadControlByte(&command, 1000);
                if (result == 1 && command == GPT_CONTROL_STOP) {
                    break;
                }
                if (result < 0 || (result == 1 && command != GPT_CONTROL_STOP)) {
                    controlLost = YES;
                    break;
                }
                elapsedSeconds += 1;
            }
            if (!delegate.didStop) {
                guestShutdownAcknowledged =
                    GPTGuestRequestShutdown(guestSocketDelegate, challenge);
                expectedGuestStop = guestShutdownAcknowledged;
            }
            BOOL stopped = GPTStopVirtualMachine(virtualMachine, delegate, queue);
            if (!stopped) {
                GPTWriteEvent(
                    GPTIsolatedHelperEventFailure,
                    GPTIsolatedHelperFailureStopFailed);
                return GPTIsolatedHelperFailureStopFailed;
            }
            if (guestStopped && !expectedGuestStop) {
                GPTWriteEvent(
                    GPTIsolatedHelperEventFailure,
                    GPTIsolatedHelperFailureGuestStopped);
                return GPTIsolatedHelperFailureGuestStopped;
            }
            if (!guestShutdownAcknowledged) {
                GPTWriteEvent(
                    GPTIsolatedHelperEventFailure,
                    GPTIsolatedHelperFailureGuestProtocol);
                return GPTIsolatedHelperFailureGuestProtocol;
            }
            if (controlLost) {
                GPTWriteEvent(
                    GPTIsolatedHelperEventFailure,
                    GPTIsolatedHelperFailureControlLost);
                return GPTIsolatedHelperFailureControlLost;
            }
            GPTWriteEvent(GPTIsolatedHelperEventStopped, 0);
            return 0;
        }
        return GPTIsolatedHelperFailureVirtualizationUnavailable;
    }
}
