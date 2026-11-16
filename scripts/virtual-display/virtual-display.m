// NOT SAFE on macOS 27.3: see README.md. It left the built-in display
// mirroring a virtual one that outlived this process.
//
// A virtual display for running the bench on a Mac that is in use: a
// covered or unfocused-and-covered window is throttled by macOS, so the
// measuring window goes on a screen nobody looks at instead of over the
// person's work. Dev use only.
//
// It uses CoreGraphics' private CGVirtualDisplay classes (as DeskPad and
// Chromium's tests do); they may change or vanish with any macOS release.
// The display exists only while this process runs: stop it (Ctrl-C, kill)
// and it is gone. It changes no setting: the arrangement it applies is for
// this login session only, and it restores the real displays' positions.
//
// Build and run (see README.md):
//   clang -fobjc-arc -framework Foundation -framework CoreGraphics \
//     virtual-display.m -o target/virtual-display
//   target/virtual-display [refresh Hz, default 60]
//
// On start it records every display's origin and the main display, creates
// a 1920x1080 HiDPI display, and at once takes it out of any mirror set
// macOS put it in, puts the real displays back where they were, and places
// the new one beyond the leftmost display. If the layout still differs
// after one correction, it exits, which removes the display.
#import <Foundation/Foundation.h>
#import <CoreGraphics/CoreGraphics.h>

@interface CGVirtualDisplayDescriptor : NSObject
@property(retain, nonatomic) dispatch_queue_t queue;
@property(retain, nonatomic) NSString *name;
@property(nonatomic) unsigned int maxPixelsHigh;
@property(nonatomic) unsigned int maxPixelsWide;
@property(nonatomic) CGSize sizeInMillimeters;
@property(nonatomic) unsigned int serialNum;
@property(nonatomic) unsigned int productID;
@property(nonatomic) unsigned int vendorID;
@property(copy, nonatomic) void (^terminationHandler)(id, id);
@end
@interface CGVirtualDisplayMode : NSObject
- (instancetype)initWithWidth:(unsigned int)width height:(unsigned int)height refreshRate:(double)rate;
@end
@interface CGVirtualDisplaySettings : NSObject
@property(retain, nonatomic) NSArray *modes;
@property(nonatomic) unsigned int hiDPI;
@end
@interface CGVirtualDisplay : NSObject
- (instancetype)initWithDescriptor:(CGVirtualDisplayDescriptor *)descriptor;
- (BOOL)applySettings:(CGVirtualDisplaySettings *)settings;
@property(readonly, nonatomic) unsigned int displayID;
@end

enum { WIDTH = 1920, HEIGHT = 1080, MAX_DISPLAYS = 16 };

typedef struct {
  CGDirectDisplayID id;
  CGPoint origin;
  size_t pixelWidth, pixelHeight;
} Recorded;

static Recorded recorded[MAX_DISPLAYS];
static uint32_t recordedCount;
static CGDirectDisplayID mainID;
static CGPoint virtualOrigin;

static void record(void) {
  CGDirectDisplayID ids[MAX_DISPLAYS];
  CGGetOnlineDisplayList(MAX_DISPLAYS, ids, &recordedCount);
  mainID = CGMainDisplayID();
  CGFloat left = 0, top = 0;
  for (uint32_t i = 0; i < recordedCount; i++) {
    CGRect b = CGDisplayBounds(ids[i]);
    CGDisplayModeRef m = CGDisplayCopyDisplayMode(ids[i]);
    recorded[i] = (Recorded){ids[i], b.origin, m ? CGDisplayModeGetPixelWidth(m) : 0, m ? CGDisplayModeGetPixelHeight(m) : 0};
    if (m) CGDisplayModeRelease(m);
    if (i == 0 || b.origin.x < left) { left = b.origin.x; top = b.origin.y; }
  }
  virtualOrigin = CGPointMake(left - WIDTH, top);
}

static void arrange(CGDirectDisplayID virtualID) {
  CGDisplayConfigRef config;
  if (CGBeginDisplayConfiguration(&config) != kCGErrorSuccess) return;
  CGConfigureDisplayMirrorOfDisplay(config, virtualID, kCGNullDirectDisplay);
  for (uint32_t i = 0; i < recordedCount; i++) {
    CGConfigureDisplayMirrorOfDisplay(config, recorded[i].id, kCGNullDirectDisplay);
    CGConfigureDisplayOrigin(config, recorded[i].id, (int32_t)recorded[i].origin.x, (int32_t)recorded[i].origin.y);
  }
  CGConfigureDisplayOrigin(config, virtualID, (int32_t)virtualOrigin.x, (int32_t)virtualOrigin.y);
  CGCompleteDisplayConfiguration(config, kCGConfigureForSession);
}

static BOOL unchanged(CGDirectDisplayID virtualID) {
  if (CGMainDisplayID() != mainID || CGDisplayIsInMirrorSet(virtualID)) return NO;
  for (uint32_t i = 0; i < recordedCount; i++) {
    CGRect b = CGDisplayBounds(recorded[i].id);
    CGDisplayModeRef m = CGDisplayCopyDisplayMode(recorded[i].id);
    BOOL same = !CGDisplayIsInMirrorSet(recorded[i].id) && CGPointEqualToPoint(b.origin, recorded[i].origin)
      && m && CGDisplayModeGetPixelWidth(m) == recorded[i].pixelWidth && CGDisplayModeGetPixelHeight(m) == recorded[i].pixelHeight;
    if (m) CGDisplayModeRelease(m);
    if (!same) return NO;
  }
  return YES;
}

int main(int argc, const char **argv) {
  @autoreleasepool {
    double hz = argc > 1 ? atof(argv[1]) : 60;
    record();
    CGVirtualDisplayDescriptor *descriptor = [CGVirtualDisplayDescriptor new];
    descriptor.queue = dispatch_get_main_queue();
    descriptor.name = @"gpui-av bench";
    descriptor.maxPixelsWide = WIDTH * 2;
    descriptor.maxPixelsHigh = HEIGHT * 2;
    descriptor.sizeInMillimeters = CGSizeMake(600, 340);
    descriptor.serialNum = 0x6a7621;
    descriptor.productID = 0x1234;
    descriptor.vendorID = 0x3456;
    descriptor.terminationHandler = ^(id a, id b) { exit(0); };
    CGVirtualDisplay *display = [[CGVirtualDisplay alloc] initWithDescriptor:descriptor];
    if (!display) { fprintf(stderr, "this macOS will not create a virtual display\n"); return 1; }
    CGVirtualDisplaySettings *settings = [CGVirtualDisplaySettings new];
    settings.hiDPI = 1;
    settings.modes = @[[[CGVirtualDisplayMode alloc] initWithWidth:WIDTH height:HEIGHT refreshRate:hz]];
    if (![display applySettings:settings]) { fprintf(stderr, "the virtual display refused its settings\n"); return 1; }
    CGDirectDisplayID virtualID = display.displayID;
    arrange(virtualID);
    sleep(1);
    if (!unchanged(virtualID)) {
      arrange(virtualID);
      sleep(1);
      if (!unchanged(virtualID)) { fprintf(stderr, "the display layout changed; removing the virtual display\n"); return 2; }
    }
    CGRect b = CGDisplayBounds(virtualID);
    printf("virtual display %u at %.0f,%.0f, %.0fx%.0f points, HiDPI, %.0f Hz; stop this process to remove it\n",
           virtualID, b.origin.x, b.origin.y, b.size.width, b.size.height, hz);
    fflush(stdout);
    [[NSRunLoop mainRunLoop] run];
  }
  return 0;
}
