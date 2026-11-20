// A camera HAL for annex instances: a back and a front camera that both film a drawn picture, a
// landscape unless `annex camera` chose another, so apps can open a camera, show a preview, take
// photos and analyse frames.

#include <aidl/android/hardware/camera/common/Status.h>
#include <aidl/android/hardware/camera/device/BnCameraDevice.h>
#include <aidl/android/hardware/camera/device/BnCameraDeviceSession.h>
#include <aidl/android/hardware/camera/device/ICameraDeviceCallback.h>
#include <aidl/android/hardware/camera/provider/BnCameraProvider.h>
#include <android/binder_ibinder.h>
#include <android/binder_manager.h>
#include <android/binder_process.h>
#include <android/hardware_buffer.h>
#include <android/log.h>
#include <system/camera_metadata.h>
#include <vndk/hardware_buffer.h>

#define STB_IMAGE_WRITE_IMPLEMENTATION
#define STBI_WRITE_NO_STDIO
#include <stb_image_write.h>

#include <algorithm>
#include <array>
#include <chrono>
#include <cmath>
#include <condition_variable>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <deque>
#include <functional>
#include <map>
#include <memory>
#include <mutex>
#include <poll.h>
#include <string>
#include <sys/types.h>
#include <thread>
#include <time.h>
#include <unistd.h>
#include <vector>

using aidl::android::hardware::camera::common::CameraResourceCost;
using aidl::android::hardware::camera::common::Status;
using aidl::android::hardware::camera::common::VendorTagSection;
using aidl::android::hardware::camera::device::BnCameraDevice;
using aidl::android::hardware::camera::device::BnCameraDeviceSession;
using aidl::android::hardware::camera::device::BufferCache;
using aidl::android::hardware::camera::device::BufferStatus;
using aidl::android::hardware::camera::device::CameraMetadata;
using aidl::android::hardware::camera::device::CameraOfflineSessionInfo;
using aidl::android::hardware::camera::device::CaptureRequest;
using aidl::android::hardware::camera::device::CaptureResult;
using aidl::android::hardware::camera::device::ErrorCode;
using aidl::android::hardware::camera::device::ErrorMsg;
using aidl::android::hardware::camera::device::HalStream;
using aidl::android::hardware::camera::device::ICameraDevice;
using aidl::android::hardware::camera::device::ICameraDeviceCallback;
using aidl::android::hardware::camera::device::ICameraDeviceSession;
using aidl::android::hardware::camera::device::ICameraInjectionSession;
using aidl::android::hardware::camera::device::ICameraOfflineSession;
using aidl::android::hardware::camera::device::NotifyMsg;
using aidl::android::hardware::camera::device::RequestTemplate;
using aidl::android::hardware::camera::device::ShutterMsg;
using aidl::android::hardware::camera::device::Stream;
using aidl::android::hardware::camera::device::StreamBuffer;
using aidl::android::hardware::camera::device::StreamConfiguration;
using aidl::android::hardware::camera::device::StreamConfigurationMode;
using aidl::android::hardware::camera::device::StreamRotation;
using aidl::android::hardware::camera::device::StreamType;
using aidl::android::hardware::camera::provider::BnCameraProvider;
using aidl::android::hardware::camera::provider::CameraIdAndStreamCombination;
using aidl::android::hardware::camera::provider::ConcurrentCameraIdCombination;
using aidl::android::hardware::camera::provider::ICameraProviderCallback;
using aidl::android::hardware::common::NativeHandle;
using aidl::android::hardware::common::fmq::MQDescriptor;
using aidl::android::hardware::common::fmq::SynchronizedReadWrite;
using aidl::android::hardware::graphics::common::BufferUsage;
using aidl::android::hardware::graphics::common::PixelFormat;
using ndk::ScopedAStatus;

namespace {

constexpr const char* kTag = "annex-camera";
constexpr const char* kInstance = "android.hardware.camera.provider.ICameraProvider/internal/0";
constexpr const char* kDevicePrefix = "device@1.1/internal/";
constexpr uid_t kRootUid = 0;
constexpr uid_t kSystemUid = 1000;
constexpr uid_t kCameraServerUid = 1047;
constexpr int32_t kArrayWidth = 1920;
constexpr int32_t kArrayHeight = 1440;
constexpr int32_t kMaxJpegSize = kArrayWidth * kArrayHeight * 3 / 2;
constexpr int64_t kFrameDuration = 33333333;
constexpr int32_t kMaxBuffers = 4;
constexpr float kFocalLength = 4.0f;
constexpr float kAperture = 2.0f;

struct Size {
    int32_t width;
    int32_t height;
};

constexpr Size kSizes[] = {
    {1920, 1440}, {1920, 1080}, {1280, 960}, {1280, 720}, {640, 480}, {320, 240}, {176, 144},
};

struct Facing {
    uint8_t lens;
    int32_t orientation;
};

constexpr Facing kFacings[] = {
    {ANDROID_LENS_FACING_BACK, 90},
    {ANDROID_LENS_FACING_FRONT, 270},
};

// The AIDL CameraBlob, as the framework reads it from the last bytes of a JPEG buffer.
struct JpegTrailer {
    int32_t blobId;
    int32_t blobSizeBytes;
};

constexpr int32_t kJpegBlobId = 0xff;

ScopedAStatus error(Status status) {
    return ScopedAStatus::fromServiceSpecificError(static_cast<int32_t>(status));
}

// SELinux is off in Redroid, so any process can get this HAL from servicemanager; only the
// camera service may drive it.
bool isCallerTrusted() {
    uid_t uid = AIBinder_getCallingUid();
    return uid == kRootUid || uid == kSystemUid || uid == kCameraServerUid;
}

int64_t bootTimeNanos() {
    timespec now{};
    clock_gettime(CLOCK_BOOTTIME, &now);
    return int64_t(now.tv_sec) * 1000000000 + now.tv_nsec;
}

class Metadata {
  public:
    Metadata() : raw(allocate_camera_metadata(64, 4096)) {}

    explicit Metadata(const std::vector<uint8_t>& bytes) : Metadata() {
        if (bytes.empty()) return;
        std::vector<uint8_t> aligned(bytes);
        auto* source = reinterpret_cast<camera_metadata_t*>(aligned.data());
        size_t size = aligned.size();
        if (validate_camera_metadata_structure(source, &size) != 0) {
            __android_log_print(ANDROID_LOG_WARN, kTag, "ignoring malformed settings");
            return;
        }
        merge(source);
    }

    Metadata(const Metadata& other) : Metadata() { merge(other.raw); }

    Metadata& operator=(const Metadata& other) {
        if (this != &other) {
            free_camera_metadata(raw);
            raw = allocate_camera_metadata(64, 4096);
            merge(other.raw);
        }
        return *this;
    }

    ~Metadata() { free_camera_metadata(raw); }

    bool isEmpty() const { return get_camera_metadata_entry_count(raw) == 0; }

    template <typename T>
    void set(uint32_t tag, std::initializer_list<T> values) {
        set(tag, values.begin(), values.size());
    }

    template <typename T>
    void set(uint32_t tag, const std::vector<T>& values) {
        set(tag, values.data(), values.size());
    }

    template <typename T>
    void set(uint32_t tag, const T* values, size_t count) {
        camera_metadata_entry_t entry{};
        int result;
        if (find_camera_metadata_entry(raw, tag, &entry) == 0) {
            result = update_camera_metadata_entry(raw, entry.index, values, count, nullptr);
        } else {
            reserve(1, calculate_camera_metadata_entry_data_size(get_camera_metadata_tag_type(tag), count));
            result = add_camera_metadata_entry(raw, tag, values, count);
        }
        if (result != 0) {
            __android_log_print(ANDROID_LOG_ERROR, kTag, "could not set %s",
                                get_camera_metadata_tag_name(tag));
        }
    }

    uint8_t byte(uint32_t tag, uint8_t fallback) const {
        camera_metadata_ro_entry_t entry{};
        bool isSet = find_camera_metadata_ro_entry(raw, tag, &entry) == 0 && entry.count > 0;
        return isSet ? entry.data.u8[0] : fallback;
    }

    int32_t integer(uint32_t tag, int32_t fallback) const {
        camera_metadata_ro_entry_t entry{};
        bool isSet = find_camera_metadata_ro_entry(raw, tag, &entry) == 0 && entry.count > 0;
        return isSet ? entry.data.i32[0] : fallback;
    }

    std::vector<int32_t> integers(uint32_t tag) const {
        camera_metadata_ro_entry_t entry{};
        if (find_camera_metadata_ro_entry(raw, tag, &entry) != 0) return {};
        return std::vector<int32_t>(entry.data.i32, entry.data.i32 + entry.count);
    }

    bool has(uint32_t tag) const {
        camera_metadata_ro_entry_t entry{};
        return find_camera_metadata_ro_entry(raw, tag, &entry) == 0;
    }

    std::vector<int32_t> tags() const {
        std::vector<int32_t> tags;
        for (size_t index = 0; index < get_camera_metadata_entry_count(raw); index++) {
            camera_metadata_ro_entry_t entry{};
            get_camera_metadata_ro_entry(raw, index, &entry);
            tags.push_back(int32_t(entry.tag));
        }
        return tags;
    }

    CameraMetadata toAidl() const {
        auto* bytes = reinterpret_cast<const uint8_t*>(raw);
        return CameraMetadata{std::vector<uint8_t>(bytes, bytes + get_camera_metadata_size(raw))};
    }

  private:
    camera_metadata_t* raw;

    void reserve(size_t entries, size_t data) {
        size_t entryCapacity = get_camera_metadata_entry_capacity(raw);
        size_t dataCapacity = get_camera_metadata_data_capacity(raw);
        size_t entryCount = get_camera_metadata_entry_count(raw);
        size_t dataCount = get_camera_metadata_data_count(raw);
        if (entryCount + entries <= entryCapacity && dataCount + data <= dataCapacity) return;
        camera_metadata_t* larger = allocate_camera_metadata(
            std::max(entryCapacity * 2, entryCount + entries), std::max(dataCapacity * 2, dataCount + data));
        append_camera_metadata(larger, raw);
        free_camera_metadata(raw);
        raw = larger;
    }

    void merge(const camera_metadata_t* source) {
        reserve(get_camera_metadata_entry_count(source), get_camera_metadata_data_count(source));
        append_camera_metadata(raw, source);
    }
};

// The pictures the cameras can film, in the order `annex camera` lists them. The setting holds the
// picture's name; anything else, or no setting, films the landscape.
enum class Picture { Landscape, City, Desk, Chart };
constexpr const char* kPictureSetting = "/data/local/tmp/annex-camera";

Picture readPicture() {
    char name[16] = {};
    if (FILE* file = std::fopen(kPictureSetting, "r")) {
        std::fscanf(file, "%15s", name);
        std::fclose(file);
    }
    if (std::strcmp(name, "city") == 0) return Picture::City;
    if (std::strcmp(name, "desk") == 0) return Picture::Desk;
    if (std::strcmp(name, "chart") == 0) return Picture::Chart;
    return Picture::Landscape;
}

// The picture both cameras film, drawn upright at any size.
class Scene {
  public:
    std::vector<uint8_t> rgb;
    std::vector<uint8_t> y;
    std::vector<uint8_t> u;
    std::vector<uint8_t> v;
    int32_t width;
    int32_t height;

    // Renders the scene as the sensor sees it: rotated by the sensor's orientation, so that the
    // framework's rotation turns it upright.
    Scene(int32_t width, int32_t height, int32_t orientation, Picture picture) : width(width), height(height) {
        int32_t uprightWidth = orientation % 180 == 0 ? width : height;
        int32_t uprightHeight = orientation % 180 == 0 ? height : width;
        rgb.resize(size_t(width) * height * 3);
        for (int32_t row = 0; row < height; row++) {
            for (int32_t column = 0; column < width; column++) {
                int32_t x = column;
                int32_t y = row;
                if (orientation == 90) {
                    x = height - 1 - row;
                    y = column;
                } else if (orientation == 270) {
                    x = row;
                    y = width - 1 - column;
                } else if (orientation == 180) {
                    x = width - 1 - column;
                    y = height - 1 - row;
                }
                uint8_t* pixel = &rgb[(size_t(row) * width + column) * 3];
                shade(picture, float(x) + 0.5f, float(y) + 0.5f, uprightWidth, uprightHeight, pixel);
            }
        }
        convertToYuv();
    }

  private:
    struct Color {
        float red;
        float green;
        float blue;
    };

    static float smoothStep(float edge0, float edge1, float value) {
        float t = std::clamp((value - edge0) / (edge1 - edge0), 0.0f, 1.0f);
        return t * t * (3 - 2 * t);
    }

    static Color mix(Color from, Color to, float amount) {
        return {from.red + (to.red - from.red) * amount, from.green + (to.green - from.green) * amount,
                from.blue + (to.blue - from.blue) * amount};
    }

    static float ridge(float x, float base, float a, float b, float c) {
        return base + 0.035f * std::sin(x * a + 1.3f) + 0.02f * std::sin(x * b + 0.4f) +
               0.01f * std::sin(x * c + 2.1f);
    }

    // A well-mixed hash of up to three integers, in [0, 1), for repeatable variety.
    static float hash(int32_t a, int32_t b = 0, int32_t c = 0) {
        uint32_t h = uint32_t(a) * 0x8da6b343u ^ uint32_t(b) * 0xd8163841u ^ uint32_t(c) * 0xcb1ab31fu;
        h ^= h >> 15;
        h *= 0x2c1b3c6du;
        h ^= h >> 12;
        h *= 0x297a2d39u;
        h ^= h >> 15;
        return float(h & 0xffffffu) / float(0x1000000u);
    }

    // 1 from `from` to `to`, 0 outside, with edges a pixel wide so they stay sharp but don't alias.
    static float band(float value, float from, float to, float pixel) {
        return smoothStep(from - pixel * 0.5f, from + pixel * 0.5f, value) *
               (1.0f - smoothStep(to - pixel * 0.5f, to + pixel * 0.5f, value));
    }

    // x and y are in pixels of the upright picture; shapes are laid out in units of its height,
    // centred, so they keep their proportions at every aspect ratio. The upright picture is
    // usually portrait, so every picture fits in a width of 0.75.
    static void shade(Picture picture, float x, float y, int32_t width, int32_t height, uint8_t* out) {
        float scale = float(height);
        float pixel = 1.0f / scale;
        float u = (x - width / 2.0f) / scale;
        float v = y / scale;
        float halfWidth = width / 2.0f / scale;
        Color color{};
        switch (picture) {
            case Picture::Landscape:
                color = landscape(u, v, pixel * 1.5f);
                break;
            case Picture::City:
                color = city(u, v, pixel);
                break;
            case Picture::Desk:
                color = desk(u, v, pixel);
                break;
            case Picture::Chart:
                color = chart(u, v, halfWidth, pixel);
                break;
        }
        out[0] = uint8_t(std::clamp(color.red, 0.0f, 1.0f) * 255.0f + 0.5f);
        out[1] = uint8_t(std::clamp(color.green, 0.0f, 1.0f) * 255.0f + 0.5f);
        out[2] = uint8_t(std::clamp(color.blue, 0.0f, 1.0f) * 255.0f + 0.5f);
    }

    // A landscape at dusk: soft, with no hard edges.
    static Color landscape(float u, float v, float pixel) {
        Color color = mix({0.20f, 0.36f, 0.66f}, {0.98f, 0.76f, 0.55f}, smoothStep(0.0f, 0.62f, v));
        float sunDistance = std::hypot(u - 0.12f, v - 0.42f);
        color = mix(color, {1.0f, 0.88f, 0.62f}, 0.45f * std::exp(-sunDistance * 9.0f));
        color = mix(color, {1.0f, 0.96f, 0.84f}, 1.0f - smoothStep(0.055f, 0.055f + pixel, sunDistance));
        for (const auto& cloud : {std::array<float, 3>{-0.22f, 0.18f, 0.11f}, {0.30f, 0.12f, 0.08f}}) {
            float stretch = std::hypot((u - cloud[0]) * 0.55f, (v - cloud[1]) * 2.2f);
            color = mix(color, {1.0f, 0.97f, 0.95f}, 0.8f * (1.0f - smoothStep(cloud[2] * 0.5f, cloud[2], stretch)));
        }

        struct Layer {
            float base;
            float a;
            float b;
            float c;
            Color color;
        };
        const Layer layers[] = {
            {0.56f, 7.0f, 13.0f, 29.0f, {0.47f, 0.47f, 0.66f}},
            {0.66f, 4.5f, 11.0f, 23.0f, {0.27f, 0.42f, 0.42f}},
            {0.78f, 3.0f, 8.0f, 19.0f, {0.18f, 0.34f, 0.22f}},
        };
        for (const Layer& layer : layers) {
            float top = ridge(u, layer.base, layer.a, layer.b, layer.c);
            Color shaded = mix(layer.color, {0.05f, 0.09f, 0.08f}, smoothStep(top, top + 0.6f, v) * 0.6f);
            color = mix(color, shaded, smoothStep(top, top + pixel, v));
        }
        return color;
    }

    // A city at night: a skyline with lit windows over a road with lane markings, all hard edges
    // and strong contrast.
    static Color city(float u, float v, float pixel) {
        constexpr float kGround = 0.86f;
        Color color = mix({0.02f, 0.04f, 0.12f}, {0.20f, 0.10f, 0.28f}, smoothStep(0.0f, 0.75f, v));
        int32_t starColumn = int32_t(std::floor(u * 60.0f));
        int32_t starRow = int32_t(std::floor(v * 60.0f));
        if (v < 0.5f && hash(starColumn, starRow, 7) > 0.96f) {
            float star = std::hypot(u - (starColumn + 0.5f) / 60.0f, v - (starRow + 0.5f) / 60.0f);
            color = mix(color, {1.0f, 1.0f, 1.0f}, 1.0f - smoothStep(0.002f, 0.002f + pixel, star));
        }
        float moon = std::hypot(u + 0.2f, v - 0.14f);
        color = mix(color, {0.96f, 0.94f, 0.86f}, 1.0f - smoothStep(0.05f, 0.05f + pixel, moon));

        // A dim row of buildings behind, and a lit one in front.
        for (int32_t layer = 0; layer < 2; layer++) {
            bool isFront = layer == 1;
            float slot = isFront ? 0.11f : 0.07f;
            int32_t index = int32_t(std::floor(u / slot));
            float local = u / slot - float(index);
            float margin = 0.04f + 0.12f * hash(index, layer, 1);
            float height = isFront ? 0.18f + 0.36f * hash(index, layer, 2) : 0.30f + 0.30f * hash(index, layer, 2);
            float top = kGround - height;
            float coverage = band(local, margin, 1.0f - margin, pixel / slot) *
                             smoothStep(top - pixel * 0.5f, top + pixel * 0.5f, v);
            if (coverage <= 0.0f) continue;
            Color wall = isFront ? Color{0.12f, 0.12f, 0.16f} : Color{0.07f, 0.07f, 0.13f};
            if (isFront) {
                constexpr int32_t kColumns = 5;
                constexpr float kFloor = 0.03f;
                float inner = (1.0f - 2.0f * margin) * slot;
                float windowU = (local - margin) * slot / inner * kColumns;
                float windowV = (v - top) / kFloor;
                int32_t column = int32_t(std::floor(windowU));
                int32_t row = int32_t(std::floor(windowV));
                bool isWindow = column >= 0 && column < kColumns && row >= 1 && v < kGround - kFloor;
                if (isWindow && hash(index, column, row) > 0.45f) {
                    float across = band(windowU - column, 0.25f, 0.75f, pixel / inner * kColumns);
                    float down = band(windowV - row, 0.30f, 0.75f, pixel / kFloor);
                    wall = mix(wall, {1.0f, 0.82f, 0.42f}, across * down);
                }
            }
            color = mix(color, wall, coverage);
        }

        if (v >= kGround) {
            color = mix({0.30f, 0.30f, 0.32f}, {0.10f, 0.10f, 0.11f}, smoothStep(kGround, kGround + pixel, v - 0.012f));
            float dash = band(u * 6.0f - std::floor(u * 6.0f), 0.0f, 0.5f, pixel * 6.0f);
            color = mix(color, {0.95f, 0.95f, 0.90f}, dash * band(v, 0.925f, 0.935f, pixel));
        }
        return color;
    }

    // A printed page on a wooden desk: a title, lines of words and a ruled table, for apps that
    // find documents or read text.
    static Color desk(float u, float v, float pixel) {
        constexpr float kHalfWidth = 0.27f;
        constexpr float kTop = 0.1f;
        constexpr float kBottom = 0.9f;
        float grain = 0.5f + 0.5f * std::sin(v * 140.0f + 4.0f * std::sin(u * 9.0f + v * 3.0f));
        Color color = mix({0.42f, 0.27f, 0.16f}, {0.52f, 0.35f, 0.21f}, grain);
        float shadow = band(u, -kHalfWidth + 0.012f, kHalfWidth + 0.012f, 0.03f) *
                       band(v, kTop + 0.012f, kBottom + 0.012f, 0.03f);
        color = mix(color, {0.15f, 0.09f, 0.05f}, 0.5f * shadow);
        float page = band(u, -kHalfWidth, kHalfWidth, pixel) * band(v, kTop, kBottom, pixel);
        if (page <= 0.0f) return color;

        float left = -kHalfWidth + 0.04f;
        float right = kHalfWidth - 0.04f;
        float ink = band(u, left, left + 0.30f, pixel) * band(v, 0.15f, 0.18f, pixel);
        constexpr float kFirstLine = 0.23f;
        constexpr float kLineGap = 0.03f;
        int32_t line = int32_t(std::floor((v - kFirstLine) / kLineGap));
        if (v >= kFirstLine && line < 12) {
            float lineTop = kFirstLine + float(line) * kLineGap;
            float across = band(v, lineTop, lineTop + 0.012f, pixel);
            // Every fourth line ends a paragraph, short.
            float end = line % 4 == 3 ? right - 0.1f - 0.25f * hash(line, 3) : right;
            float start = left;
            for (int32_t word = 0; start < end; word++) {
                float stop = std::min(start + 0.03f + 0.07f * hash(line, word, 5), end);
                ink = std::max(ink, across * band(u, start, stop, pixel));
                start = stop + 0.018f;
            }
        }
        constexpr float kTableTop = 0.62f;
        constexpr float kTableBottom = 0.84f;
        constexpr float kRule = 0.004f;
        if (v > kTableTop - kRule && v < kTableBottom + kRule && u > left - kRule && u < right + kRule) {
            for (int32_t k = 0; k <= 4; k++) {
                float rowLine = kTableTop + (kTableBottom - kTableTop) * float(k) / 4.0f;
                float columnLine = left + (right - left) * float(k) / 4.0f;
                ink = std::max(ink, band(v, rowLine - kRule / 2, rowLine + kRule / 2, pixel) *
                                        band(u, left, right, pixel));
                ink = std::max(ink, band(u, columnLine - kRule / 2, columnLine + kRule / 2, pixel) *
                                        band(v, kTableTop, kTableBottom, pixel));
            }
        }
        Color paper = mix({0.97f, 0.97f, 0.95f}, {0.08f, 0.08f, 0.10f}, ink);
        return mix(color, paper, page);
    }

    // A test chart: colour bars, a Siemens star, a checkerboard and a grey ramp, for checking focus,
    // colour and exposure.
    static Color chart(float u, float v, float halfWidth, float pixel) {
        constexpr Color kBars[] = {{1, 1, 1}, {1, 1, 0}, {0, 1, 1}, {0, 1, 0},
                                   {1, 0, 1}, {1, 0, 0}, {0, 0, 1}, {0, 0, 0}};
        Color color{0.5f, 0.5f, 0.5f};
        float left = -halfWidth * 0.9f;
        float right = halfWidth * 0.9f;
        float across = (u - left) / (right - left);
        float inside = band(u, left, right, pixel);
        if (v > 0.04f && v < 0.28f) {
            int32_t bar = std::clamp(int32_t(across * 8.0f), 0, 7);
            color = mix(color, kBars[bar], inside * band(v, 0.05f, 0.27f, pixel));
        }

        float radius = std::min(0.2f, halfWidth * 0.55f);
        float dx = u;
        float dy = v - 0.52f;
        float distance = std::hypot(dx, dy);
        if (distance < radius + pixel) {
            constexpr float kSpokes = 18.0f;
            float spoke = std::sin(std::atan2(dy, dx) * kSpokes);
            float spokePixel = kSpokes * pixel / std::max(distance, pixel);
            Color star = mix({0, 0, 0}, {1, 1, 1}, smoothStep(-spokePixel, spokePixel, spoke));
            star = mix(star, {0.5f, 0.5f, 0.5f}, 1.0f - smoothStep(0.012f, 0.012f + pixel, distance));
            color = mix(color, star, 1.0f - smoothStep(radius - pixel * 0.5f, radius + pixel * 0.5f, distance));
        }

        if (v > 0.76f && v < 0.86f) {
            constexpr float kCell = 0.025f;
            int32_t column = int32_t(std::floor((u - left) / kCell));
            int32_t row = int32_t(std::floor((v - 0.77f) / kCell));
            Color square = (column + row) % 2 == 0 ? Color{0, 0, 0} : Color{1, 1, 1};
            color = mix(color, square, inside * band(v, 0.77f, 0.85f, pixel));
        }
        if (v > 0.86f && v < 0.98f) {
            // Outlined, or the middle step would vanish into the background's grey.
            float frame = band(u, left - 0.006f, right + 0.006f, pixel) * band(v, 0.874f, 0.966f, pixel);
            color = mix(color, {0, 0, 0}, frame);
            float step = float(std::clamp(int32_t(across * 11.0f), 0, 10)) / 10.0f;
            color = mix(color, {step, step, step}, inside * band(v, 0.88f, 0.96f, pixel));
        }
        return color;
    }

    // Full-range BT.601, the JFIF dataspace cameras report for YUV_420_888.
    void convertToYuv() {
        int32_t chromaWidth = (width + 1) / 2;
        int32_t chromaHeight = (height + 1) / 2;
        y.resize(size_t(width) * height);
        u.resize(size_t(chromaWidth) * chromaHeight);
        v.resize(size_t(chromaWidth) * chromaHeight);
        for (size_t index = 0; index < y.size(); index++) {
            const uint8_t* pixel = &rgb[index * 3];
            y[index] = uint8_t(std::clamp(0.299f * pixel[0] + 0.587f * pixel[1] + 0.114f * pixel[2], 0.0f, 255.0f));
        }
        for (int32_t row = 0; row < chromaHeight; row++) {
            for (int32_t column = 0; column < chromaWidth; column++) {
                float red = 0, green = 0, blue = 0;
                int count = 0;
                for (int32_t dy = 0; dy < 2; dy++) {
                    for (int32_t dx = 0; dx < 2; dx++) {
                        int32_t sourceRow = std::min(row * 2 + dy, height - 1);
                        int32_t sourceColumn = std::min(column * 2 + dx, width - 1);
                        const uint8_t* pixel = &rgb[(size_t(sourceRow) * width + sourceColumn) * 3];
                        red += pixel[0];
                        green += pixel[1];
                        blue += pixel[2];
                        count++;
                    }
                }
                red /= count;
                green /= count;
                blue /= count;
                size_t index = size_t(row) * chromaWidth + column;
                u[index] = uint8_t(std::clamp(128 - 0.168736f * red - 0.331264f * green + 0.5f * blue, 0.0f, 255.0f));
                v[index] = uint8_t(std::clamp(128 + 0.5f * red - 0.418688f * green - 0.081312f * blue, 0.0f, 255.0f));
            }
        }
    }
};

void appendBytes(void* context, void* data, int size) {
    auto* out = static_cast<std::vector<uint8_t>*>(context);
    auto* bytes = static_cast<uint8_t*>(data);
    out->insert(out->end(), bytes, bytes + size);
}

uint16_t exifOrientation(int32_t degrees) {
    switch (((degrees % 360) + 360) % 360) {
        case 90:
            return 6;
        case 180:
            return 3;
        case 270:
            return 8;
        default:
            return 1;
    }
}

// stb writes SOI and a JFIF APP0 segment first; this returns what follows them.
std::vector<uint8_t> encodeBody(const uint8_t* rgb, int32_t width, int32_t height, int quality) {
    std::vector<uint8_t> encoded;
    stbi_write_jpg_to_func(appendBytes, &encoded, width, height, 3, rgb, std::clamp(quality, 1, 100));
    if (encoded.size() < 6) return {};
    size_t body = 2;
    if (encoded[2] == 0xff && encoded[3] == 0xe0) body = 4 + ((size_t(encoded[4]) << 8) | encoded[5]);
    return std::vector<uint8_t>(encoded.begin() + body, encoded.end());
}

std::vector<uint8_t> thumbnail(const Scene& scene, int32_t width, int32_t height, int quality) {
    std::vector<uint8_t> rgb(size_t(width) * height * 3);
    for (int32_t row = 0; row < height; row++) {
        for (int32_t column = 0; column < width; column++) {
            int32_t sourceRow = int32_t((int64_t(row) * 2 + 1) * scene.height / (int64_t(height) * 2));
            int32_t sourceColumn = int32_t((int64_t(column) * 2 + 1) * scene.width / (int64_t(width) * 2));
            memcpy(&rgb[(size_t(row) * width + column) * 3],
                   &scene.rgb[(size_t(sourceRow) * scene.width + sourceColumn) * 3], 3);
        }
    }
    std::vector<uint8_t> body = encodeBody(rgb.data(), width, height, quality);
    if (body.empty()) return {};
    std::vector<uint8_t> jpeg = {0xff, 0xd8};
    jpeg.insert(jpeg.end(), body.begin(), body.end());
    return jpeg;
}

void putShort(std::vector<uint8_t>& out, uint16_t value) {
    out.insert(out.end(), {uint8_t(value), uint8_t(value >> 8)});
}

void putLong(std::vector<uint8_t>& out, uint32_t value) {
    out.insert(out.end(), {uint8_t(value), uint8_t(value >> 8), uint8_t(value >> 16), uint8_t(value >> 24)});
}

void putEntry(std::vector<uint8_t>& out, uint16_t tag, uint16_t type, uint32_t value) {
    putShort(out, tag);
    putShort(out, type);
    putLong(out, 1);
    if (type == 3) {
        putShort(out, uint16_t(value));
        putShort(out, 0);
    } else {
        putLong(out, value);
    }
}

// A little-endian TIFF block with the orientation in IFD0 and, when asked for, a JPEG thumbnail
// in IFD1, which is all the EXIF that apps and the camera framework read from a capture.
std::vector<uint8_t> exifSegment(uint16_t orientation, const std::vector<uint8_t>& thumbnail) {
    constexpr uint16_t kShort = 3;
    constexpr uint16_t kLong = 4;
    constexpr uint32_t kFirstIfd = 8;
    constexpr uint32_t kSecondIfd = kFirstIfd + 2 + 12 + 4;
    constexpr uint32_t kThumbnail = kSecondIfd + 2 + 3 * 12 + 4;
    std::vector<uint8_t> tiff = {'I', 'I', 0x2a, 0x00};
    putLong(tiff, kFirstIfd);
    putShort(tiff, 1);
    putEntry(tiff, 0x0112, kShort, orientation);
    putLong(tiff, thumbnail.empty() ? 0 : kSecondIfd);
    if (!thumbnail.empty()) {
        putShort(tiff, 3);
        putEntry(tiff, 0x0103, kShort, 6);
        putEntry(tiff, 0x0201, kLong, kThumbnail);
        putEntry(tiff, 0x0202, kLong, uint32_t(thumbnail.size()));
        putLong(tiff, 0);
        tiff.insert(tiff.end(), thumbnail.begin(), thumbnail.end());
    }
    size_t length = 2 + 6 + tiff.size();
    std::vector<uint8_t> segment = {0xff, 0xe1, uint8_t(length >> 8), uint8_t(length), 'E', 'x', 'i', 'f', 0, 0};
    segment.insert(segment.end(), tiff.begin(), tiff.end());
    return segment;
}

// A JPEG whose EXIF orientation asks viewers to turn it upright, as phone cameras do instead of
// rotating the pixels.
std::vector<uint8_t> encodeJpeg(const Scene& scene, const Metadata& settings) {
    std::vector<uint8_t> body = encodeBody(scene.rgb.data(), scene.width, scene.height,
                                           settings.byte(ANDROID_JPEG_QUALITY, 90));
    if (body.empty()) return {};
    std::vector<int32_t> thumbnailSize = settings.integers(ANDROID_JPEG_THUMBNAIL_SIZE);
    std::vector<uint8_t> small;
    if (thumbnailSize.size() == 2 && thumbnailSize[0] > 0 && thumbnailSize[1] > 0) {
        small = thumbnail(scene, thumbnailSize[0], thumbnailSize[1], settings.byte(ANDROID_JPEG_THUMBNAIL_QUALITY, 90));
        if (small.size() > 60000) small.clear();
    }
    uint16_t orientation = exifOrientation(settings.integer(ANDROID_JPEG_ORIENTATION, 0));
    std::vector<uint8_t> exif = exifSegment(orientation, small);
    std::vector<uint8_t> jpeg = {0xff, 0xd8};
    jpeg.insert(jpeg.end(), exif.begin(), exif.end());
    jpeg.insert(jpeg.end(), body.begin(), body.end());
    return jpeg;
}

Metadata characteristics(const Facing& facing) {
    Metadata metadata;
    std::vector<int32_t> configurations;
    std::vector<int64_t> durations;
    std::vector<int64_t> stalls;
    const int32_t formats[] = {int32_t(PixelFormat::IMPLEMENTATION_DEFINED), int32_t(PixelFormat::YCBCR_420_888),
                               int32_t(PixelFormat::BLOB)};
    for (int32_t format : formats) {
        for (const Size& size : kSizes) {
            int64_t stall = format == int32_t(PixelFormat::BLOB) ? kFrameDuration : 0;
            configurations.insert(configurations.end(), {format, size.width, size.height,
                                                         ANDROID_SCALER_AVAILABLE_STREAM_CONFIGURATIONS_OUTPUT});
            durations.insert(durations.end(), {format, size.width, size.height, kFrameDuration});
            stalls.insert(stalls.end(), {format, size.width, size.height, stall});
        }
    }
    const camera_metadata_rational_t step{1, 1};

    metadata.set<uint8_t>(ANDROID_COLOR_CORRECTION_AVAILABLE_ABERRATION_MODES, {ANDROID_COLOR_CORRECTION_ABERRATION_MODE_OFF});
    metadata.set<uint8_t>(ANDROID_CONTROL_AE_AVAILABLE_ANTIBANDING_MODES,
                          {ANDROID_CONTROL_AE_ANTIBANDING_MODE_OFF, ANDROID_CONTROL_AE_ANTIBANDING_MODE_AUTO});
    metadata.set<uint8_t>(ANDROID_CONTROL_AE_AVAILABLE_MODES, {ANDROID_CONTROL_AE_MODE_ON});
    metadata.set<int32_t>(ANDROID_CONTROL_AE_AVAILABLE_TARGET_FPS_RANGES, {15, 30, 30, 30});
    metadata.set<int32_t>(ANDROID_CONTROL_AE_COMPENSATION_RANGE, {0, 0});
    metadata.set(ANDROID_CONTROL_AE_COMPENSATION_STEP, &step, 1);
    metadata.set<uint8_t>(ANDROID_CONTROL_AE_LOCK_AVAILABLE, {ANDROID_CONTROL_AE_LOCK_AVAILABLE_FALSE});
    metadata.set<uint8_t>(ANDROID_CONTROL_AF_AVAILABLE_MODES, {ANDROID_CONTROL_AF_MODE_OFF});
    metadata.set<uint8_t>(ANDROID_CONTROL_AVAILABLE_EFFECTS, {ANDROID_CONTROL_EFFECT_MODE_OFF});
    metadata.set<uint8_t>(ANDROID_CONTROL_AVAILABLE_MODES, {ANDROID_CONTROL_MODE_AUTO});
    metadata.set<uint8_t>(ANDROID_CONTROL_AVAILABLE_SCENE_MODES, {ANDROID_CONTROL_SCENE_MODE_DISABLED});
    metadata.set<uint8_t>(ANDROID_CONTROL_AVAILABLE_VIDEO_STABILIZATION_MODES, {ANDROID_CONTROL_VIDEO_STABILIZATION_MODE_OFF});
    metadata.set<uint8_t>(ANDROID_CONTROL_AWB_AVAILABLE_MODES, {ANDROID_CONTROL_AWB_MODE_AUTO});
    metadata.set<uint8_t>(ANDROID_CONTROL_AWB_LOCK_AVAILABLE, {ANDROID_CONTROL_AWB_LOCK_AVAILABLE_FALSE});
    metadata.set<int32_t>(ANDROID_CONTROL_MAX_REGIONS, {0, 0, 0});
    metadata.set<uint8_t>(ANDROID_DISTORTION_CORRECTION_AVAILABLE_MODES, {ANDROID_DISTORTION_CORRECTION_MODE_OFF});
    metadata.set<uint8_t>(ANDROID_EDGE_AVAILABLE_EDGE_MODES, {ANDROID_EDGE_MODE_OFF});
    metadata.set<uint8_t>(ANDROID_FLASH_INFO_AVAILABLE, {ANDROID_FLASH_INFO_AVAILABLE_FALSE});
    metadata.set<uint8_t>(ANDROID_HOT_PIXEL_AVAILABLE_HOT_PIXEL_MODES, {ANDROID_HOT_PIXEL_MODE_OFF});
    metadata.set<uint8_t>(ANDROID_INFO_SUPPORTED_HARDWARE_LEVEL, {ANDROID_INFO_SUPPORTED_HARDWARE_LEVEL_LIMITED});
    metadata.set<int32_t>(ANDROID_JPEG_AVAILABLE_THUMBNAIL_SIZES, {0, 0, 160, 90, 160, 120});
    metadata.set<int32_t>(ANDROID_JPEG_MAX_SIZE, {kMaxJpegSize});
    metadata.set<uint8_t>(ANDROID_LENS_FACING, {facing.lens});
    metadata.set<float>(ANDROID_LENS_INFO_AVAILABLE_APERTURES, {kAperture});
    metadata.set<float>(ANDROID_LENS_INFO_AVAILABLE_FILTER_DENSITIES, {0.0f});
    metadata.set<float>(ANDROID_LENS_INFO_AVAILABLE_FOCAL_LENGTHS, {kFocalLength});
    metadata.set<uint8_t>(ANDROID_LENS_INFO_AVAILABLE_OPTICAL_STABILIZATION, {ANDROID_LENS_OPTICAL_STABILIZATION_MODE_OFF});
    metadata.set<uint8_t>(ANDROID_LENS_INFO_FOCUS_DISTANCE_CALIBRATION,
                          {ANDROID_LENS_INFO_FOCUS_DISTANCE_CALIBRATION_UNCALIBRATED});
    metadata.set<float>(ANDROID_LENS_INFO_MINIMUM_FOCUS_DISTANCE, {0.0f});
    metadata.set<uint8_t>(ANDROID_NOISE_REDUCTION_AVAILABLE_NOISE_REDUCTION_MODES, {ANDROID_NOISE_REDUCTION_MODE_OFF});
    metadata.set<uint8_t>(ANDROID_REQUEST_AVAILABLE_CAPABILITIES,
                          {ANDROID_REQUEST_AVAILABLE_CAPABILITIES_BACKWARD_COMPATIBLE});
    metadata.set<int32_t>(ANDROID_REQUEST_MAX_NUM_OUTPUT_STREAMS, {0, 3, 1});
    metadata.set<int32_t>(ANDROID_REQUEST_PARTIAL_RESULT_COUNT, {1});
    metadata.set<uint8_t>(ANDROID_REQUEST_PIPELINE_MAX_DEPTH, {uint8_t(kMaxBuffers)});
    metadata.set<float>(ANDROID_SCALER_AVAILABLE_MAX_DIGITAL_ZOOM, {1.0f});
    metadata.set(ANDROID_SCALER_AVAILABLE_STREAM_CONFIGURATIONS, configurations);
    metadata.set(ANDROID_SCALER_AVAILABLE_MIN_FRAME_DURATIONS, durations);
    metadata.set(ANDROID_SCALER_AVAILABLE_STALL_DURATIONS, stalls);
    metadata.set<uint8_t>(ANDROID_SCALER_CROPPING_TYPE, {ANDROID_SCALER_CROPPING_TYPE_CENTER_ONLY});
    metadata.set<int32_t>(ANDROID_SENSOR_AVAILABLE_TEST_PATTERN_MODES, {ANDROID_SENSOR_TEST_PATTERN_MODE_OFF});
    metadata.set<int32_t>(ANDROID_SENSOR_INFO_ACTIVE_ARRAY_SIZE, {0, 0, kArrayWidth, kArrayHeight});
    metadata.set<int32_t>(ANDROID_SENSOR_INFO_PIXEL_ARRAY_SIZE, {kArrayWidth, kArrayHeight});
    metadata.set<int32_t>(ANDROID_SENSOR_INFO_PRE_CORRECTION_ACTIVE_ARRAY_SIZE, {0, 0, kArrayWidth, kArrayHeight});
    metadata.set<float>(ANDROID_SENSOR_INFO_PHYSICAL_SIZE, {4.8f, 3.6f});
    metadata.set<uint8_t>(ANDROID_SENSOR_INFO_TIMESTAMP_SOURCE, {ANDROID_SENSOR_INFO_TIMESTAMP_SOURCE_REALTIME});
    metadata.set<int32_t>(ANDROID_SENSOR_ORIENTATION, {facing.orientation});
    metadata.set<uint8_t>(ANDROID_SHADING_AVAILABLE_MODES, {ANDROID_SHADING_MODE_OFF});
    metadata.set<uint8_t>(ANDROID_STATISTICS_INFO_AVAILABLE_FACE_DETECT_MODES, {ANDROID_STATISTICS_FACE_DETECT_MODE_OFF});
    metadata.set<uint8_t>(ANDROID_STATISTICS_INFO_AVAILABLE_HOT_PIXEL_MAP_MODES,
                          {ANDROID_STATISTICS_HOT_PIXEL_MAP_MODE_OFF});
    metadata.set<uint8_t>(ANDROID_STATISTICS_INFO_AVAILABLE_LENS_SHADING_MAP_MODES,
                          {ANDROID_STATISTICS_LENS_SHADING_MAP_MODE_OFF});
    metadata.set<int32_t>(ANDROID_STATISTICS_INFO_MAX_FACE_COUNT, {0});
    metadata.set<int32_t>(ANDROID_SYNC_MAX_LATENCY, {ANDROID_SYNC_MAX_LATENCY_UNKNOWN});
    return metadata;
}

Metadata requestTemplate(uint8_t intent) {
    Metadata metadata;
    bool isVideo = intent == ANDROID_CONTROL_CAPTURE_INTENT_VIDEO_RECORD ||
                   intent == ANDROID_CONTROL_CAPTURE_INTENT_VIDEO_SNAPSHOT;
    metadata.set<uint8_t>(ANDROID_COLOR_CORRECTION_ABERRATION_MODE, {ANDROID_COLOR_CORRECTION_ABERRATION_MODE_OFF});
    metadata.set<uint8_t>(ANDROID_CONTROL_AE_ANTIBANDING_MODE, {ANDROID_CONTROL_AE_ANTIBANDING_MODE_AUTO});
    metadata.set<int32_t>(ANDROID_CONTROL_AE_EXPOSURE_COMPENSATION, {0});
    metadata.set<uint8_t>(ANDROID_CONTROL_AE_LOCK, {ANDROID_CONTROL_AE_LOCK_OFF});
    metadata.set<uint8_t>(ANDROID_CONTROL_AE_MODE, {ANDROID_CONTROL_AE_MODE_ON});
    metadata.set<uint8_t>(ANDROID_CONTROL_AE_PRECAPTURE_TRIGGER, {ANDROID_CONTROL_AE_PRECAPTURE_TRIGGER_IDLE});
    metadata.set<int32_t>(ANDROID_CONTROL_AE_TARGET_FPS_RANGE, {isVideo ? 30 : 15, 30});
    metadata.set<uint8_t>(ANDROID_CONTROL_AF_MODE, {ANDROID_CONTROL_AF_MODE_OFF});
    metadata.set<uint8_t>(ANDROID_CONTROL_AF_TRIGGER, {ANDROID_CONTROL_AF_TRIGGER_IDLE});
    metadata.set<uint8_t>(ANDROID_CONTROL_AWB_LOCK, {ANDROID_CONTROL_AWB_LOCK_OFF});
    metadata.set<uint8_t>(ANDROID_CONTROL_AWB_MODE, {ANDROID_CONTROL_AWB_MODE_AUTO});
    metadata.set<uint8_t>(ANDROID_CONTROL_CAPTURE_INTENT, {intent});
    metadata.set<uint8_t>(ANDROID_CONTROL_EFFECT_MODE, {ANDROID_CONTROL_EFFECT_MODE_OFF});
    metadata.set<uint8_t>(ANDROID_CONTROL_MODE, {ANDROID_CONTROL_MODE_AUTO});
    metadata.set<uint8_t>(ANDROID_CONTROL_SCENE_MODE, {ANDROID_CONTROL_SCENE_MODE_DISABLED});
    metadata.set<uint8_t>(ANDROID_CONTROL_VIDEO_STABILIZATION_MODE, {ANDROID_CONTROL_VIDEO_STABILIZATION_MODE_OFF});
    metadata.set<uint8_t>(ANDROID_DISTORTION_CORRECTION_MODE, {ANDROID_DISTORTION_CORRECTION_MODE_OFF});
    metadata.set<uint8_t>(ANDROID_EDGE_MODE, {ANDROID_EDGE_MODE_OFF});
    metadata.set<uint8_t>(ANDROID_FLASH_MODE, {ANDROID_FLASH_MODE_OFF});
    metadata.set<uint8_t>(ANDROID_HOT_PIXEL_MODE, {ANDROID_HOT_PIXEL_MODE_OFF});
    metadata.set<int32_t>(ANDROID_JPEG_ORIENTATION, {0});
    metadata.set<uint8_t>(ANDROID_JPEG_QUALITY, {90});
    metadata.set<uint8_t>(ANDROID_JPEG_THUMBNAIL_QUALITY, {90});
    metadata.set<int32_t>(ANDROID_JPEG_THUMBNAIL_SIZE, {0, 0});
    metadata.set<float>(ANDROID_LENS_APERTURE, {kAperture});
    metadata.set<float>(ANDROID_LENS_FILTER_DENSITY, {0.0f});
    metadata.set<float>(ANDROID_LENS_FOCAL_LENGTH, {kFocalLength});
    metadata.set<float>(ANDROID_LENS_FOCUS_DISTANCE, {0.0f});
    metadata.set<uint8_t>(ANDROID_LENS_OPTICAL_STABILIZATION_MODE, {ANDROID_LENS_OPTICAL_STABILIZATION_MODE_OFF});
    metadata.set<uint8_t>(ANDROID_NOISE_REDUCTION_MODE, {ANDROID_NOISE_REDUCTION_MODE_OFF});
    metadata.set<int32_t>(ANDROID_SCALER_CROP_REGION, {0, 0, kArrayWidth, kArrayHeight});
    metadata.set<int32_t>(ANDROID_SENSOR_TEST_PATTERN_MODE, {ANDROID_SENSOR_TEST_PATTERN_MODE_OFF});
    metadata.set<uint8_t>(ANDROID_SHADING_MODE, {ANDROID_SHADING_MODE_OFF});
    metadata.set<uint8_t>(ANDROID_STATISTICS_FACE_DETECT_MODE, {ANDROID_STATISTICS_FACE_DETECT_MODE_OFF});
    metadata.set<uint8_t>(ANDROID_STATISTICS_HOT_PIXEL_MAP_MODE, {ANDROID_STATISTICS_HOT_PIXEL_MAP_MODE_OFF});
    metadata.set<uint8_t>(ANDROID_STATISTICS_LENS_SHADING_MAP_MODE, {ANDROID_STATISTICS_LENS_SHADING_MAP_MODE_OFF});
    return metadata;
}

const uint32_t kResultOnlyKeys[] = {
    ANDROID_CONTROL_AE_STATE,      ANDROID_CONTROL_AF_STATE,         ANDROID_CONTROL_AWB_STATE,
    ANDROID_FLASH_STATE,           ANDROID_LENS_STATE,               ANDROID_REQUEST_PIPELINE_DEPTH,
    ANDROID_SENSOR_EXPOSURE_TIME,  ANDROID_SENSOR_FRAME_DURATION,    ANDROID_SENSOR_ROLLING_SHUTTER_SKEW,
    ANDROID_SENSOR_SENSITIVITY,    ANDROID_SENSOR_TIMESTAMP,         ANDROID_STATISTICS_SCENE_FLICKER,
};

Metadata deviceCharacteristics(const Facing& facing) {
    Metadata metadata = characteristics(facing);
    std::vector<int32_t> requestKeys = requestTemplate(ANDROID_CONTROL_CAPTURE_INTENT_PREVIEW).tags();
    std::vector<int32_t> resultKeys = requestKeys;
    resultKeys.insert(resultKeys.end(), std::begin(kResultOnlyKeys), std::end(kResultOnlyKeys));
    metadata.set(ANDROID_REQUEST_AVAILABLE_REQUEST_KEYS, requestKeys);
    metadata.set(ANDROID_REQUEST_AVAILABLE_RESULT_KEYS, resultKeys);
    std::vector<int32_t> characteristicsKeys = metadata.tags();
    characteristicsKeys.push_back(ANDROID_REQUEST_AVAILABLE_CHARACTERISTICS_KEYS);
    metadata.set(ANDROID_REQUEST_AVAILABLE_CHARACTERISTICS_KEYS, characteristicsKeys);
    return metadata;
}

bool isSupported(const Stream& stream) {
    bool isKnownFormat = stream.format == PixelFormat::IMPLEMENTATION_DEFINED ||
                         stream.format == PixelFormat::YCBCR_420_888 || stream.format == PixelFormat::BLOB;
    bool isKnownSize = std::any_of(std::begin(kSizes), std::end(kSizes), [&](const Size& size) {
        return size.width == stream.width && size.height == stream.height;
    });
    return stream.streamType == StreamType::OUTPUT && stream.rotation == StreamRotation::ROTATION_0 &&
           isKnownFormat && isKnownSize;
}

bool isSupported(const StreamConfiguration& configuration) {
    if (configuration.operationMode != StreamConfigurationMode::NORMAL_MODE) return false;
    int processed = 0;
    int stalling = 0;
    for (const Stream& stream : configuration.streams) {
        if (!isSupported(stream)) return false;
        (stream.format == PixelFormat::BLOB ? stalling : processed)++;
    }
    return !configuration.streams.empty() && processed <= 3 && stalling <= 1;
}

// Wraps the fds a request carries without taking them over; the import clones them.
AHardwareBuffer* importBuffer(const NativeHandle& handle, const Stream& stream, PixelFormat format) {
    size_t fdCount = handle.fds.size();
    size_t intCount = handle.ints.size();
    auto* raw = static_cast<native_handle_t*>(
        malloc(sizeof(native_handle_t) + sizeof(int) * (fdCount + intCount)));
    raw->version = sizeof(native_handle_t);
    raw->numFds = int(fdCount);
    raw->numInts = int(intCount);
    for (size_t index = 0; index < fdCount; index++) raw->data[index] = handle.fds[index].get();
    std::copy(handle.ints.begin(), handle.ints.end(), raw->data + fdCount);

    bool isBlob = format == PixelFormat::BLOB;
    AHardwareBuffer_Desc description{};
    description.width = isBlob ? uint32_t(stream.bufferSize > 0 ? stream.bufferSize : kMaxJpegSize)
                               : uint32_t(stream.width);
    description.height = isBlob ? 1 : uint32_t(stream.height);
    description.layers = 1;
    description.format = uint32_t(format);
    description.usage = uint64_t(stream.usage) | AHARDWAREBUFFER_USAGE_CPU_WRITE_OFTEN;
    description.stride = description.width;
    AHardwareBuffer* buffer = nullptr;
    int result = AHardwareBuffer_createFromHandle(&description, raw,
                                                  AHARDWAREBUFFER_CREATE_FROM_HANDLE_METHOD_CLONE, &buffer);
    free(raw);
    if (result != 0) {
        __android_log_print(ANDROID_LOG_ERROR, kTag, "could not import a buffer for stream %d: %d", stream.id,
                            result);
        return nullptr;
    }
    return buffer;
}

void waitForFence(int fd) {
    if (fd < 0) return;
    pollfd poller{fd, POLLIN, 0};
    if (poll(&poller, 1, 1000) <= 0) {
        __android_log_print(ANDROID_LOG_WARN, kTag, "an acquire fence did not signal within 1 s");
    }
    close(fd);
}

bool writeYuv(AHardwareBuffer* buffer, const Scene& scene) {
    AHardwareBuffer_Planes planes{};
    if (AHardwareBuffer_lockPlanes(buffer, AHARDWAREBUFFER_USAGE_CPU_WRITE_OFTEN, -1, nullptr, &planes) != 0 ||
        planes.planeCount < 3) {
        return false;
    }
    for (int32_t row = 0; row < scene.height; row++) {
        auto* line = static_cast<uint8_t*>(planes.planes[0].data) + size_t(row) * planes.planes[0].rowStride;
        memcpy(line, &scene.y[size_t(row) * scene.width], scene.width);
    }
    int32_t chromaWidth = (scene.width + 1) / 2;
    int32_t chromaHeight = (scene.height + 1) / 2;
    const std::vector<uint8_t>* sources[] = {&scene.u, &scene.v};
    for (int plane = 1; plane <= 2; plane++) {
        const AHardwareBuffer_Plane& target = planes.planes[plane];
        const std::vector<uint8_t>& source = *sources[plane - 1];
        for (int32_t row = 0; row < chromaHeight; row++) {
            auto* line = static_cast<uint8_t*>(target.data) + size_t(row) * target.rowStride;
            for (int32_t column = 0; column < chromaWidth; column++) {
                line[size_t(column) * target.pixelStride] = source[size_t(row) * chromaWidth + column];
            }
        }
    }
    AHardwareBuffer_unlock(buffer, nullptr);
    return true;
}

bool writeJpeg(AHardwareBuffer* buffer, const Scene& scene, const Metadata& settings) {
    AHardwareBuffer_Desc description{};
    AHardwareBuffer_describe(buffer, &description);
    std::vector<uint8_t> jpeg = encodeJpeg(scene, settings);
    size_t capacity = description.width;
    if (jpeg.empty() || jpeg.size() + sizeof(JpegTrailer) > capacity) {
        __android_log_print(ANDROID_LOG_ERROR, kTag, "a %zu byte JPEG does not fit a %zu byte buffer",
                            jpeg.size(), capacity);
        return false;
    }
    void* address = nullptr;
    if (AHardwareBuffer_lock(buffer, AHARDWAREBUFFER_USAGE_CPU_WRITE_OFTEN, -1, nullptr, &address) != 0) {
        return false;
    }
    auto* bytes = static_cast<uint8_t*>(address);
    memcpy(bytes, jpeg.data(), jpeg.size());
    JpegTrailer trailer{kJpegBlobId, int32_t(jpeg.size())};
    memcpy(bytes + capacity - sizeof(trailer), &trailer, sizeof(trailer));
    AHardwareBuffer_unlock(buffer, nullptr);
    return true;
}

class Session : public BnCameraDeviceSession {
  public:
    Session(const std::shared_ptr<ICameraDeviceCallback>& callback, const Facing& facing,
            std::function<void()> onClosed)
        : callback(callback), facing(facing), onClosed(std::move(onClosed)) {
        worker = std::thread([this] { run(); });
    }

    ~Session() override { stop(); }

    ScopedAStatus close() override {
        stop();
        return ScopedAStatus::ok();
    }

    ScopedAStatus configureStreams(const StreamConfiguration& configuration,
                                   std::vector<HalStream>* halStreams) override {
        if (!isSupported(configuration)) return error(Status::ILLEGAL_ARGUMENT);
        std::lock_guard lock(mutex);
        if (!pending.empty() || isCapturing) return error(Status::INTERNAL_ERROR);
        std::map<int32_t, StreamState> configured;
        halStreams->clear();
        for (const Stream& stream : configuration.streams) {
            // gralloc picks a format for IMPLEMENTATION_DEFINED that the HAL can't lock, so the
            // HAL decides: NV12, which displays, encodes and is what it writes anyway.
            PixelFormat format = stream.format == PixelFormat::IMPLEMENTATION_DEFINED ? PixelFormat::YCBCR_420_888
                                                                                     : stream.format;
            StreamState state{stream, format, {}};
            auto previous = streams.find(stream.id);
            if (previous != streams.end() && previous->second.stream.width == stream.width &&
                previous->second.stream.height == stream.height && previous->second.format == format) {
                state.buffers = std::move(previous->second.buffers);
            }
            configured.emplace(stream.id, std::move(state));
            HalStream halStream;
            halStream.id = stream.id;
            halStream.overrideFormat = format;
            halStream.producerUsage = BufferUsage::CPU_WRITE_OFTEN;
            halStream.consumerUsage = BufferUsage(0);
            halStream.maxBuffers = kMaxBuffers;
            halStream.overrideDataSpace = stream.dataSpace;
            halStream.supportOffline = false;
            halStreams->push_back(halStream);
        }
        for (auto& [id, state] : streams) releaseBuffers(state);
        streams = std::move(configured);
        scenes.clear();
        return ScopedAStatus::ok();
    }

    ScopedAStatus constructDefaultRequestSettings(RequestTemplate type, CameraMetadata* settings) override {
        uint8_t intent;
        switch (type) {
            case RequestTemplate::PREVIEW:
                intent = ANDROID_CONTROL_CAPTURE_INTENT_PREVIEW;
                break;
            case RequestTemplate::STILL_CAPTURE:
                intent = ANDROID_CONTROL_CAPTURE_INTENT_STILL_CAPTURE;
                break;
            case RequestTemplate::VIDEO_RECORD:
                intent = ANDROID_CONTROL_CAPTURE_INTENT_VIDEO_RECORD;
                break;
            case RequestTemplate::VIDEO_SNAPSHOT:
                intent = ANDROID_CONTROL_CAPTURE_INTENT_VIDEO_SNAPSHOT;
                break;
            default:
                return error(Status::ILLEGAL_ARGUMENT);
        }
        *settings = requestTemplate(intent).toAidl();
        return ScopedAStatus::ok();
    }

    ScopedAStatus flush() override {
        std::unique_lock lock(mutex);
        isFlushing = true;
        wake.notify_all();
        drained.wait(lock, [this] { return pending.empty() && !isCapturing; });
        isFlushing = false;
        return ScopedAStatus::ok();
    }

    ScopedAStatus getCaptureRequestMetadataQueue(MQDescriptor<int8_t, SynchronizedReadWrite>*) override {
        return ScopedAStatus::ok();
    }

    ScopedAStatus getCaptureResultMetadataQueue(MQDescriptor<int8_t, SynchronizedReadWrite>*) override {
        return ScopedAStatus::ok();
    }

    ScopedAStatus isReconfigurationRequired(const CameraMetadata&, const CameraMetadata&, bool* isRequired) override {
        *isRequired = false;
        return ScopedAStatus::ok();
    }

    ScopedAStatus processCaptureRequest(const std::vector<CaptureRequest>& requests,
                                        const std::vector<BufferCache>& cachesToRemove, int32_t* count) override {
        std::lock_guard lock(mutex);
        *count = 0;
        if (isClosed) return error(Status::INTERNAL_ERROR);
        for (const BufferCache& cache : cachesToRemove) {
            auto stream = streams.find(cache.streamId);
            if (stream == streams.end()) continue;
            auto buffer = stream->second.buffers.find(cache.bufferId);
            if (buffer == stream->second.buffers.end()) continue;
            AHardwareBuffer_release(buffer->second);
            stream->second.buffers.erase(buffer);
        }
        for (const CaptureRequest& request : requests) {
            if (!request.settings.metadata.empty()) {
                settings = Metadata(request.settings.metadata);
            } else if (settings.isEmpty()) {
                return error(Status::ILLEGAL_ARGUMENT);
            }
            if (request.inputBuffer.streamId != -1 || request.outputBuffers.empty()) {
                return error(Status::ILLEGAL_ARGUMENT);
            }
            Pending capture{request.frameNumber, settings, {}};
            for (const StreamBuffer& output : request.outputBuffers) {
                auto stream = streams.find(output.streamId);
                if (stream == streams.end()) return error(Status::ILLEGAL_ARGUMENT);
                StreamState& state = stream->second;
                AHardwareBuffer*& cached = state.buffers[output.bufferId];
                if (!output.buffer.fds.empty()) {
                    if (cached != nullptr) AHardwareBuffer_release(cached);
                    cached = importBuffer(output.buffer, state.stream, state.format);
                }
                int fence = output.acquireFence.fds.empty() ? -1 : dup(output.acquireFence.fds[0].get());
                capture.outputs.push_back({output.streamId, output.bufferId, cached, fence});
            }
            pending.push_back(std::move(capture));
            ++*count;
        }
        wake.notify_all();
        return ScopedAStatus::ok();
    }

    ScopedAStatus signalStreamFlush(const std::vector<int32_t>&, int32_t) override { return ScopedAStatus::ok(); }

    ScopedAStatus switchToOffline(const std::vector<int32_t>&, CameraOfflineSessionInfo*,
                                  std::shared_ptr<ICameraOfflineSession>*) override {
        return error(Status::OPERATION_NOT_SUPPORTED);
    }

    ScopedAStatus repeatingRequestEnd(int32_t, const std::vector<int32_t>&) override { return ScopedAStatus::ok(); }

  private:
    struct StreamState {
        Stream stream;
        PixelFormat format;
        std::map<int64_t, AHardwareBuffer*> buffers;
    };

    struct Output {
        int32_t streamId;
        int64_t bufferId;
        AHardwareBuffer* buffer;
        int acquireFence;
    };

    struct Pending {
        int32_t frameNumber;
        Metadata settings;
        std::vector<Output> outputs;
    };

    std::map<std::pair<int32_t, int32_t>, std::shared_ptr<Scene>> scenes;
    std::map<int32_t, StreamState> streams;
    std::shared_ptr<ICameraDeviceCallback> callback;
    std::condition_variable drained;
    std::condition_variable wake;
    std::function<void()> onClosed;
    std::deque<Pending> pending;
    std::thread worker;
    std::mutex mutex;
    Metadata settings;
    Facing facing;
    // Read when the camera opens, so a change made with `annex camera` shows from the next open.
    Picture picture = readPicture();
    bool isCapturing = false;
    bool isFlushing = false;
    bool isClosed = false;
    bool isFocusLocked = false;

    static void releaseBuffers(StreamState& state) {
        for (auto& [id, buffer] : state.buffers) {
            if (buffer != nullptr) AHardwareBuffer_release(buffer);
        }
        state.buffers.clear();
    }

    void stop() {
        {
            std::lock_guard lock(mutex);
            if (isClosed) return;
            isClosed = true;
            isFlushing = true;
            wake.notify_all();
        }
        if (worker.joinable()) worker.join();
        std::lock_guard lock(mutex);
        for (auto& [id, state] : streams) releaseBuffers(state);
        streams.clear();
        scenes.clear();
        onClosed();
    }

    std::shared_ptr<Scene> scene(int32_t width, int32_t height) {
        auto& cached = scenes[{width, height}];
        if (cached == nullptr) cached = std::make_shared<Scene>(width, height, facing.orientation, picture);
        return cached;
    }

    void run() {
        auto nextFrame = std::chrono::steady_clock::now();
        std::unique_lock lock(mutex);
        while (true) {
            wake.wait(lock, [this] { return isClosed || !pending.empty(); });
            if (pending.empty()) break;
            if (!isFlushing) {
                wake.wait_until(lock, nextFrame, [this] { return isFlushing; });
            }
            Pending capture = std::move(pending.front());
            pending.pop_front();
            isCapturing = true;
            bool isError = isFlushing;
            std::map<int32_t, PixelFormat> formats;
            std::map<int32_t, std::shared_ptr<Scene>> outputScenes;
            if (!isError) {
                for (const Output& output : capture.outputs) {
                    const StreamState& state = streams.at(output.streamId);
                    formats[output.streamId] = state.format;
                    outputScenes[output.streamId] = scene(state.stream.width, state.stream.height);
                }
            }
            lock.unlock();
            if (isError) {
                fail(capture);
            } else {
                deliver(capture, formats, outputScenes);
                nextFrame = std::max(nextFrame + std::chrono::nanoseconds(kFrameDuration),
                                     std::chrono::steady_clock::now());
            }
            lock.lock();
            isCapturing = false;
            if (pending.empty()) drained.notify_all();
        }
        drained.notify_all();
    }

    void fail(Pending& capture) {
        NotifyMsg message;
        message.set<NotifyMsg::error>(ErrorMsg{capture.frameNumber, -1, ErrorCode::ERROR_REQUEST});
        callback->notify({message});
        CaptureResult result;
        result.frameNumber = capture.frameNumber;
        result.inputBuffer.streamId = -1;
        for (const Output& output : capture.outputs) {
            if (output.acquireFence >= 0) ::close(output.acquireFence);
            StreamBuffer buffer;
            buffer.streamId = output.streamId;
            buffer.bufferId = output.bufferId;
            buffer.status = BufferStatus::ERROR;
            result.outputBuffers.push_back(std::move(buffer));
        }
        std::vector<CaptureResult> results;
        results.push_back(std::move(result));
        callback->processCaptureResult(results);
    }

    void deliver(Pending& capture, const std::map<int32_t, PixelFormat>& formats,
                 const std::map<int32_t, std::shared_ptr<Scene>>& outputScenes) {
        int64_t timestamp = bootTimeNanos();
        NotifyMsg shutter;
        shutter.set<NotifyMsg::shutter>(ShutterMsg{capture.frameNumber, timestamp, timestamp});
        callback->notify({shutter});

        CaptureResult result;
        result.frameNumber = capture.frameNumber;
        result.inputBuffer.streamId = -1;
        result.partialResult = 1;
        std::vector<NotifyMsg> errors;
        for (const Output& output : capture.outputs) {
            waitForFence(output.acquireFence);
            const Scene& outputScene = *outputScenes.at(output.streamId);
            PixelFormat format = formats.at(output.streamId);
            bool isWritten = output.buffer != nullptr &&
                             (format == PixelFormat::BLOB ? writeJpeg(output.buffer, outputScene, capture.settings)
                                                          : writeYuv(output.buffer, outputScene));
            if (!isWritten) {
                NotifyMsg message;
                message.set<NotifyMsg::error>(ErrorMsg{capture.frameNumber, output.streamId, ErrorCode::ERROR_BUFFER});
                errors.push_back(message);
            }
            StreamBuffer buffer;
            buffer.streamId = output.streamId;
            buffer.bufferId = output.bufferId;
            buffer.status = isWritten ? BufferStatus::OK : BufferStatus::ERROR;
            result.outputBuffers.push_back(std::move(buffer));
        }
        if (!errors.empty()) callback->notify(errors);
        result.result = resultMetadata(capture.settings, timestamp).toAidl();
        std::vector<CaptureResult> results;
        results.push_back(std::move(result));
        callback->processCaptureResult(results);
    }

    // A fixed-focus lens, but apps that ask to focus anyway are told it locked, so they don't
    // wait forever.
    Metadata resultMetadata(const Metadata& request, int64_t timestamp) {
        Metadata result = request;
        uint8_t afMode = request.byte(ANDROID_CONTROL_AF_MODE, ANDROID_CONTROL_AF_MODE_OFF);
        uint8_t afTrigger = request.byte(ANDROID_CONTROL_AF_TRIGGER, ANDROID_CONTROL_AF_TRIGGER_IDLE);
        if (afTrigger == ANDROID_CONTROL_AF_TRIGGER_START) isFocusLocked = true;
        if (afTrigger == ANDROID_CONTROL_AF_TRIGGER_CANCEL) isFocusLocked = false;
        uint8_t afState = ANDROID_CONTROL_AF_STATE_INACTIVE;
        if (afMode != ANDROID_CONTROL_AF_MODE_OFF && afMode != ANDROID_CONTROL_AF_MODE_EDOF) {
            bool isContinuous = afMode == ANDROID_CONTROL_AF_MODE_CONTINUOUS_PICTURE ||
                                afMode == ANDROID_CONTROL_AF_MODE_CONTINUOUS_VIDEO;
            if (isFocusLocked) {
                afState = ANDROID_CONTROL_AF_STATE_FOCUSED_LOCKED;
            } else if (isContinuous) {
                afState = ANDROID_CONTROL_AF_STATE_PASSIVE_FOCUSED;
            }
        }
        bool isAeOn = request.byte(ANDROID_CONTROL_AE_MODE, ANDROID_CONTROL_AE_MODE_ON) != ANDROID_CONTROL_AE_MODE_OFF;
        result.set<uint8_t>(ANDROID_CONTROL_AE_STATE,
                            {isAeOn ? uint8_t(ANDROID_CONTROL_AE_STATE_CONVERGED) : uint8_t(ANDROID_CONTROL_AE_STATE_INACTIVE)});
        result.set<uint8_t>(ANDROID_CONTROL_AF_STATE, {afState});
        result.set<uint8_t>(ANDROID_CONTROL_AWB_STATE, {ANDROID_CONTROL_AWB_STATE_CONVERGED});
        result.set<uint8_t>(ANDROID_FLASH_STATE, {ANDROID_FLASH_STATE_UNAVAILABLE});
        result.set<uint8_t>(ANDROID_LENS_STATE, {ANDROID_LENS_STATE_STATIONARY});
        result.set<uint8_t>(ANDROID_REQUEST_PIPELINE_DEPTH, {1});
        result.set<int64_t>(ANDROID_SENSOR_EXPOSURE_TIME, {kFrameDuration / 2});
        result.set<int64_t>(ANDROID_SENSOR_FRAME_DURATION, {kFrameDuration});
        result.set<int64_t>(ANDROID_SENSOR_ROLLING_SHUTTER_SKEW, {0});
        result.set<int32_t>(ANDROID_SENSOR_SENSITIVITY, {100});
        result.set<int64_t>(ANDROID_SENSOR_TIMESTAMP, {timestamp});
        result.set<uint8_t>(ANDROID_STATISTICS_SCENE_FLICKER, {ANDROID_STATISTICS_SCENE_FLICKER_NONE});
        if (!request.has(ANDROID_SCALER_CROP_REGION)) {
            result.set<int32_t>(ANDROID_SCALER_CROP_REGION, {0, 0, kArrayWidth, kArrayHeight});
        }
        return result;
    }
};

class Device : public BnCameraDevice {
  public:
    explicit Device(const Facing& facing) : facing(facing) {}

    ScopedAStatus getCameraCharacteristics(CameraMetadata* metadata) override {
        *metadata = deviceCharacteristics(facing).toAidl();
        return ScopedAStatus::ok();
    }

    ScopedAStatus getPhysicalCameraCharacteristics(const std::string&, CameraMetadata*) override {
        return error(Status::ILLEGAL_ARGUMENT);
    }

    ScopedAStatus getResourceCost(CameraResourceCost* cost) override {
        cost->resourceCost = 50;
        cost->conflictingDevices.clear();
        return ScopedAStatus::ok();
    }

    ScopedAStatus isStreamCombinationSupported(const StreamConfiguration& configuration, bool* isSupported) override {
        *isSupported = ::isSupported(configuration);
        return ScopedAStatus::ok();
    }

    ScopedAStatus open(const std::shared_ptr<ICameraDeviceCallback>& callback,
                       std::shared_ptr<ICameraDeviceSession>* session) override {
        if (!isCallerTrusted()) return ScopedAStatus::fromExceptionCode(EX_SECURITY);
        if (callback == nullptr) return error(Status::ILLEGAL_ARGUMENT);
        std::lock_guard lock(mutex);
        if (isOpen) return error(Status::CAMERA_IN_USE);
        isOpen = true;
        *session = ndk::SharedRefBase::make<Session>(callback, facing, [this] {
            std::lock_guard lock(mutex);
            isOpen = false;
        });
        return ScopedAStatus::ok();
    }

    ScopedAStatus openInjectionSession(const std::shared_ptr<ICameraDeviceCallback>&,
                                       std::shared_ptr<ICameraInjectionSession>*) override {
        return error(Status::OPERATION_NOT_SUPPORTED);
    }

    ScopedAStatus setTorchMode(bool) override { return error(Status::OPERATION_NOT_SUPPORTED); }

    ScopedAStatus turnOnTorchWithStrengthLevel(int32_t) override { return error(Status::OPERATION_NOT_SUPPORTED); }

    ScopedAStatus getTorchStrengthLevel(int32_t*) override { return error(Status::OPERATION_NOT_SUPPORTED); }

  private:
    std::mutex mutex;
    Facing facing;
    bool isOpen = false;
};

class Provider : public BnCameraProvider {
  public:
    Provider() {
        for (const Facing& facing : kFacings) devices.push_back(ndk::SharedRefBase::make<Device>(facing));
    }

    ScopedAStatus setCallback(const std::shared_ptr<ICameraProviderCallback>&) override {
        if (!isCallerTrusted()) return ScopedAStatus::fromExceptionCode(EX_SECURITY);
        return ScopedAStatus::ok();
    }

    ScopedAStatus getVendorTags(std::vector<VendorTagSection>* sections) override {
        sections->clear();
        return ScopedAStatus::ok();
    }

    ScopedAStatus getCameraIdList(std::vector<std::string>* names) override {
        names->clear();
        for (size_t index = 0; index < devices.size(); index++) {
            names->push_back(kDevicePrefix + std::to_string(index));
        }
        return ScopedAStatus::ok();
    }

    ScopedAStatus getCameraDeviceInterface(const std::string& name, std::shared_ptr<ICameraDevice>* device) override {
        if (!isCallerTrusted()) return ScopedAStatus::fromExceptionCode(EX_SECURITY);
        for (size_t index = 0; index < devices.size(); index++) {
            if (name == kDevicePrefix + std::to_string(index)) {
                *device = devices[index];
                return ScopedAStatus::ok();
            }
        }
        return error(Status::ILLEGAL_ARGUMENT);
    }

    ScopedAStatus notifyDeviceStateChange(int64_t) override { return ScopedAStatus::ok(); }

    ScopedAStatus getConcurrentCameraIds(std::vector<ConcurrentCameraIdCombination>* combinations) override {
        combinations->clear();
        return ScopedAStatus::ok();
    }

    ScopedAStatus isConcurrentStreamCombinationSupported(const std::vector<CameraIdAndStreamCombination>&,
                                                         bool* isSupported) override {
        *isSupported = false;
        return ScopedAStatus::ok();
    }

  private:
    std::vector<std::shared_ptr<Device>> devices;
};

}  // namespace

int main() {
    ABinderProcess_setThreadPoolMaxThreadCount(4);
    ABinderProcess_startThreadPool();
    auto provider = ndk::SharedRefBase::make<Provider>();
    if (AServiceManager_addService(provider->asBinder().get(), kInstance) != STATUS_OK) {
        __android_log_print(ANDROID_LOG_ERROR, kTag, "could not register %s", kInstance);
        return 1;
    }
    __android_log_print(ANDROID_LOG_INFO, kTag, "camera HAL ready");
    ABinderProcess_joinThreadPool();
    return 1;
}
