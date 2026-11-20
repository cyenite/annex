package dev.annex;

import android.content.Context;
import android.os.Bundle;
import android.provider.Settings;
import android.util.Log;

import java.io.File;
import java.io.FileOutputStream;
import java.lang.reflect.Field;
import java.nio.charset.StandardCharsets;
import java.security.MessageDigest;
import java.util.Map;

// Offline push tokens for the Google services stand-in. annex calls into this class from microG's
// push registration (`PushRegisterServiceKt`, `PushRegisterManager`), which upstream refuses while
// Google device registration ("checkin") is off. Once the user turns checkin on in microG's
// settings, every method here steps aside and microG registers with Google as usual.
public final class AnnexPush {
    private static final String TAG = "AnnexPush";
    // annex reads these files to answer `annex fcm token`: the token, then the sender id.
    private static final String RECORDS = "annex-push";

    private AnnexPush() {}

    public static boolean isOffline(Context context) {
        try {
            Class<?> checkin = Class.forName("org.microg.gms.checkin.CheckinPreferences");
            return !(Boolean) checkin.getMethod("isEnabled", Context.class).invoke(null, context);
        } catch (Throwable error) {
            Log.w(TAG, "could not read the checkin setting", error);
            return false;
        }
    }

    // Answers a registration request with a placeholder token, the way microG's own
    // completeRegisterRequest answers with Google's. Returns false to let microG handle it.
    public static boolean complete(
            Context context, Object database, String requestId, Object request, Object callback) {
        if (!isOffline(context)) {
            return false;
        }
        try {
            Class<?> type = request.getClass();
            String app = (String) type.getField("app").get(request);
            String sender = (String) type.getField("sender").get(request);
            boolean isDelete = type.getField("delete").getBoolean(request);
            Bundle result = new Bundle();
            File record = new File(new File(context.getFilesDir(), RECORDS), app);
            if (isDelete) {
                record.delete();
                result.putString("unregistered", withRequestId(app, requestId));
            } else {
                String token = token(context, app, sender, installationId(type, request));
                write(record, token + "\n" + (sender == null ? "" : sender) + "\n");
                result.putString("registration_id", withRequestId(token, requestId));
                Log.i(TAG, "offline push token for " + app);
            }
            Class.forName("org.microg.gms.gcm.PushRegisterManager$BundleCallback")
                    .getMethod("onResult", Bundle.class)
                    .invoke(callback, result);
            return true;
        } catch (Throwable error) {
            Log.w(TAG, "could not make an offline push token", error);
            return false;
        }
    }

    // Stable for an app on an instance: the same inputs always give the same token, so it
    // survives microG's data being cleared, and differs between instances by their Android id.
    private static String token(Context context, String app, String sender, String fid)
            throws Exception {
        String device = Settings.Secure.getString(
                context.getContentResolver(), Settings.Secure.ANDROID_ID);
        String seed = device + "|" + app + "|" + sender + "|" + fid;
        byte[] digest = MessageDigest.getInstance("SHA-256")
                .digest(seed.getBytes(StandardCharsets.UTF_8));
        StringBuilder token = new StringBuilder();
        if (fid != null) {
            token.append(fid).append(':');
        }
        token.append("annex_offline_");
        for (byte b : digest) {
            token.append(String.format("%02x", b));
        }
        return token.toString();
    }

    // Firebase sends its installation id as `appid`; microG keeps request extras as "X-<key>".
    @SuppressWarnings("unchecked")
    private static String installationId(Class<?> type, Object request) throws Exception {
        Field extras = type.getDeclaredField("extraParams");
        extras.setAccessible(true);
        return ((Map<String, String>) extras.get(request)).get("X-appid");
    }

    private static String withRequestId(String message, String requestId) {
        return requestId == null ? message : "|ID|" + requestId + "|" + message;
    }

    private static void write(File record, String content) throws Exception {
        File folder = record.getParentFile();
        if (!folder.isDirectory() && !folder.mkdirs()) {
            throw new IllegalStateException("could not create " + folder);
        }
        try (FileOutputStream out = new FileOutputStream(record)) {
            out.write(content.getBytes(StandardCharsets.UTF_8));
        }
    }
}
