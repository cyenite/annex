// A GNSS HAL for annex instances: reports the position annex is set to, through Android's real
// GPS path, so apps don't see it as a mock location.

#include <aidl/android/hardware/gnss/BnGnss.h>
#include <aidl/android/hardware/gnss/IGnssCallback.h>
#include <android/binder_ibinder.h>
#include <android/binder_manager.h>
#include <android/binder_process.h>
#include <android/log.h>

#include <algorithm>
#include <atomic>
#include <chrono>
#include <cstdio>
#include <fstream>
#include <mutex>
#include <optional>
#include <string>
#include <sys/types.h>
#include <thread>
#include <time.h>

using aidl::android::hardware::gnss::BnGnss;
using aidl::android::hardware::gnss::ElapsedRealtime;
using aidl::android::hardware::gnss::GnssLocation;
using aidl::android::hardware::gnss::IAGnss;
using aidl::android::hardware::gnss::IAGnssRil;
using aidl::android::hardware::gnss::IGnss;
using aidl::android::hardware::gnss::IGnssAntennaInfo;
using aidl::android::hardware::gnss::IGnssBatching;
using aidl::android::hardware::gnss::IGnssCallback;
using aidl::android::hardware::gnss::IGnssConfiguration;
using aidl::android::hardware::gnss::IGnssDebug;
using aidl::android::hardware::gnss::IGnssGeofence;
using aidl::android::hardware::gnss::IGnssMeasurementInterface;
using aidl::android::hardware::gnss::IGnssNavigationMessageInterface;
using aidl::android::hardware::gnss::IGnssPowerIndication;
using aidl::android::hardware::gnss::IGnssPsds;
using aidl::android::hardware::gnss::measurement_corrections::IMeasurementCorrectionsInterface;
using aidl::android::hardware::gnss::visibility_control::IGnssVisibilityControl;
using ndk::ScopedAStatus;

namespace {

constexpr const char* kTag = "annex-gnss";
constexpr uid_t kRootUid = 0;
constexpr uid_t kSystemUid = 1000;
constexpr const char* kSettingPath = "/data/local/tmp/annex-location";
constexpr const char* kHostFixPath = "/dev/annex-host-location";
constexpr double kDefaultLatitude = 37.4220;
constexpr double kDefaultLongitude = -122.0841;
constexpr double kDefaultAccuracy = 5;

struct Fix {
    double latitude;
    double longitude;
    double accuracy;
};

std::string readFirstLine(const char* path) {
    std::ifstream file(path);
    std::string line;
    std::getline(file, line);
    return line;
}

std::optional<Fix> parse(const std::string& text, double accuracy) {
    Fix fix{0, 0, accuracy};
    if (std::sscanf(text.c_str(), "%lf,%lf %lf", &fix.latitude, &fix.longitude, &fix.accuracy) < 2) {
        return std::nullopt;
    }
    return fix;
}

// The same setting `annex location` writes: empty for the default position, `host` for the
// position the desktop bridge shares, or LAT,LON.
std::optional<Fix> currentFix() {
    const std::string setting = readFirstLine(kSettingPath);
    if (setting.empty()) return Fix{kDefaultLatitude, kDefaultLongitude, kDefaultAccuracy};
    if (setting == "host") return parse(readFirstLine(kHostFixPath), kDefaultAccuracy);
    return parse(setting, kDefaultAccuracy);
}

int64_t nanosOf(clockid_t clock) {
    timespec now{};
    clock_gettime(clock, &now);
    return int64_t(now.tv_sec) * 1000000000 + now.tv_nsec;
}

GnssLocation toLocation(const Fix& fix) {
    GnssLocation location;
    location.gnssLocationFlags = GnssLocation::HAS_LAT_LONG | GnssLocation::HAS_ALTITUDE |
                                 GnssLocation::HAS_HORIZONTAL_ACCURACY |
                                 GnssLocation::HAS_VERTICAL_ACCURACY;
    location.latitudeDegrees = fix.latitude;
    location.longitudeDegrees = fix.longitude;
    location.altitudeMeters = 0;
    location.horizontalAccuracyMeters = fix.accuracy;
    location.verticalAccuracyMeters = fix.accuracy;
    location.timestampMillis = nanosOf(CLOCK_REALTIME) / 1000000;
    location.elapsedRealtime.flags =
            ElapsedRealtime::HAS_TIMESTAMP_NS | ElapsedRealtime::HAS_TIME_UNCERTAINTY_NS;
    location.elapsedRealtime.timestampNs = nanosOf(CLOCK_BOOTTIME);
    location.elapsedRealtime.timeUncertaintyNs = 1000000;
    return location;
}

// Android 15's framework calls into an extension it was handed as null, so every optional
// extension is refused with an error instead.
// Only system_server and root may drive the HAL. SELinux is off in Redroid, so servicemanager
// hands this binder to any process that asks, and a caller that replaced the callback would
// receive the position (this computer's own, with `host`) without a location permission.
bool isSystemCaller() {
    const uid_t uid = AIBinder_getCallingUid();
    return uid == kSystemUid || uid == kRootUid;
}

ScopedAStatus denied() {
    return ScopedAStatus::fromExceptionCode(EX_SECURITY);
}

ScopedAStatus okForSystem() {
    return isSystemCaller() ? ScopedAStatus::ok() : denied();
}

ScopedAStatus unsupported() {
    return ScopedAStatus::fromExceptionCode(EX_UNSUPPORTED_OPERATION);
}

class Gnss : public BnGnss {
  public:
    ~Gnss() override { stopSession(); }

    ScopedAStatus setCallback(const std::shared_ptr<IGnssCallback>& callback) override {
        if (!isSystemCaller()) return denied();
        {
            std::lock_guard<std::mutex> guard(mLock);
            mCallback = callback;
        }
        callback->gnssSetCapabilitiesCb(IGnssCallback::CAPABILITY_SCHEDULING);
        IGnssCallback::GnssSystemInfo info;
        info.yearOfHw = 2024;
        info.name = "annex";
        callback->gnssSetSystemInfoCb(info);
        return ScopedAStatus::ok();
    }

    ScopedAStatus close() override {
        if (!isSystemCaller()) return denied();
        stopSession();
        std::lock_guard<std::mutex> guard(mLock);
        mCallback = nullptr;
        return ScopedAStatus::ok();
    }

    ScopedAStatus start() override {
        if (!isSystemCaller()) return denied();
        if (mIsRunning.exchange(true)) return ScopedAStatus::ok();
        if (mThread.joinable()) mThread.join();
        report([](auto& callback) {
            callback->gnssStatusCb(IGnssCallback::GnssStatusValue::SESSION_BEGIN);
        });
        mThread = std::thread([this] { run(); });
        return ScopedAStatus::ok();
    }

    ScopedAStatus stop() override {
        if (!isSystemCaller()) return denied();
        stopSession();
        report([](auto& callback) {
            callback->gnssStatusCb(IGnssCallback::GnssStatusValue::SESSION_END);
        });
        return ScopedAStatus::ok();
    }

    ScopedAStatus setPositionMode(const PositionModeOptions& options) override {
        if (!isSystemCaller()) return denied();
        mIntervalMs = std::max(1000, options.minIntervalMs);
        return ScopedAStatus::ok();
    }

    ScopedAStatus injectTime(int64_t, int64_t, int32_t) override { return okForSystem(); }
    ScopedAStatus injectLocation(const GnssLocation&) override { return okForSystem(); }
    ScopedAStatus injectBestLocation(const GnssLocation&) override { return okForSystem(); }
    ScopedAStatus deleteAidingData(GnssAidingData) override { return okForSystem(); }
    ScopedAStatus startSvStatus() override { return okForSystem(); }
    ScopedAStatus stopSvStatus() override { return okForSystem(); }
    ScopedAStatus startNmea() override { return okForSystem(); }
    ScopedAStatus stopNmea() override { return okForSystem(); }

    ScopedAStatus getExtensionPsds(std::shared_ptr<IGnssPsds>*) override {
        return unsupported();
    }
    ScopedAStatus getExtensionGnssBatching(std::shared_ptr<IGnssBatching>*) override {
        return unsupported();
    }
    ScopedAStatus getExtensionGnssGeofence(std::shared_ptr<IGnssGeofence>*) override {
        return unsupported();
    }
    ScopedAStatus getExtensionGnssNavigationMessage(
            std::shared_ptr<IGnssNavigationMessageInterface>*) override {
        return unsupported();
    }
    ScopedAStatus getExtensionMeasurementCorrections(
            std::shared_ptr<IMeasurementCorrectionsInterface>*) override {
        return unsupported();
    }
    ScopedAStatus getExtensionGnssConfiguration(std::shared_ptr<IGnssConfiguration>*) override {
        return unsupported();
    }
    ScopedAStatus getExtensionGnssMeasurement(std::shared_ptr<IGnssMeasurementInterface>*) override {
        return unsupported();
    }
    ScopedAStatus getExtensionGnssPowerIndication(std::shared_ptr<IGnssPowerIndication>*) override {
        return unsupported();
    }
    ScopedAStatus getExtensionAGnss(std::shared_ptr<IAGnss>*) override { return unsupported(); }
    ScopedAStatus getExtensionAGnssRil(std::shared_ptr<IAGnssRil>*) override {
        return unsupported();
    }
    ScopedAStatus getExtensionGnssDebug(std::shared_ptr<IGnssDebug>*) override {
        return unsupported();
    }
    ScopedAStatus getExtensionGnssVisibilityControl(std::shared_ptr<IGnssVisibilityControl>*) override {
        return unsupported();
    }
    ScopedAStatus getExtensionGnssAntennaInfo(std::shared_ptr<IGnssAntennaInfo>*) override {
        return unsupported();
    }

  private:
    template <typename F>
    void report(F&& send) {
        std::shared_ptr<IGnssCallback> callback;
        {
            std::lock_guard<std::mutex> guard(mLock);
            callback = mCallback;
        }
        if (callback != nullptr) send(callback);
    }

    void stopSession() {
        mIsRunning = false;
        if (mThread.joinable()) mThread.join();
    }

    void run() {
        using namespace std::chrono;
        while (mIsRunning) {
            const auto next = steady_clock::now() + milliseconds(mIntervalMs.load());
            if (const auto fix = currentFix()) {
                const GnssLocation location = toLocation(*fix);
                report([&](auto& callback) { callback->gnssLocationCb(location); });
            }
            while (mIsRunning && steady_clock::now() < next) {
                std::this_thread::sleep_for(milliseconds(100));
            }
        }
    }

    std::mutex mLock;
    std::shared_ptr<IGnssCallback> mCallback;
    std::atomic<bool> mIsRunning{false};
    std::atomic<int> mIntervalMs{1000};
    std::thread mThread;
};

}  // namespace

int main() {
    ABinderProcess_setThreadPoolMaxThreadCount(1);
    ABinderProcess_startThreadPool();
    auto gnss = ndk::SharedRefBase::make<Gnss>();
    const std::string instance = std::string(IGnss::descriptor) + "/default";
    if (AServiceManager_addService(gnss->asBinder().get(), instance.c_str()) != STATUS_OK) {
        __android_log_print(ANDROID_LOG_ERROR, kTag, "could not register %s", instance.c_str());
        return 1;
    }
    __android_log_print(ANDROID_LOG_INFO, kTag, "registered %s", instance.c_str());
    ABinderProcess_joinThreadPool();
    return 1;
}
