package com.android.server.pm;

import android.content.pm.PackageInfo;
import android.content.pm.Signature;
import android.content.pm.SigningInfo;
import android.os.Bundle;
import android.util.Log;

import java.io.File;
import java.lang.reflect.Constructor;

public final class AnnexSignatures {
    private static final String TAG = "AnnexSignatures";
    // The folders annex mounts the stand-in into, read-only from the host. Only the packages
    // installed from there may borrow the certificate in their `fake-signature` meta-data; a
    // generic spoofing permission would let any installed app claim any signature, including the
    // developer's own release key. Apps can't write to /system, and an update installed over a
    // system app lives in /data, so the folder is the trust anchor, not the signing key.
    private static final String[] STAND_IN_FOLDERS = {
        "/system/priv-app/GmsCore", "/system/priv-app/Phonesky",
    };

    private AnnexSignatures() {}

    public static void apply(PackageInfo info, Object state) {
        if (info == null || !isStandInPackage(info.packageName)) {
            return;
        }
        try {
            if (!isTrustedStandIn(state)) {
                return;
            }
            Object pkg = state.getClass().getMethod("getPkg").invoke(state);
            Bundle metaData = (Bundle) pkg.getClass().getMethod("getMetaData").invoke(pkg);
            String fake = metaData == null ? null : metaData.getString("fake-signature");
            if (fake == null) {
                return;
            }
            Signature[] signatures = {new Signature(fake)};
            info.signatures = signatures;
            info.signingInfo = signingInfo(signatures);
        } catch (Throwable error) {
            Log.w(TAG, "could not stand in for " + info.packageName, error);
        }
    }

    private static boolean isStandInPackage(String name) {
        return "com.google.android.gms".equals(name) || "com.android.vending".equals(name);
    }

    private static boolean isTrustedStandIn(Object state) throws Exception {
        Class<?> type = state.getClass();
        boolean isSystem = (Boolean) type.getMethod("isSystem").invoke(state);
        boolean isUpdated = (Boolean) type.getMethod("isUpdatedSystemApp").invoke(state);
        if (!isSystem || isUpdated) {
            return false;
        }
        File path = (File) type.getMethod("getPath").invoke(state);
        for (String folder : STAND_IN_FOLDERS) {
            if (folder.equals(path.getAbsolutePath())) {
                return true;
            }
        }
        return false;
    }

    // SigningDetails and SigningInfo(SigningDetails) are hidden, but system_server may use them,
    // and unlike Android 15's public constructor they exist on 13 and 14 too. 3 is signature
    // scheme v3, which is what the real Play services package reports.
    private static SigningInfo signingInfo(Signature[] signatures) throws Exception {
        Class<?> details = Class.forName("android.content.pm.SigningDetails");
        Constructor<?> makeDetails = details.getConstructor(Signature[].class, int.class);
        Object value = makeDetails.newInstance(signatures, 3);
        Constructor<SigningInfo> makeInfo = SigningInfo.class.getConstructor(details);
        return makeInfo.newInstance(value);
    }
}
