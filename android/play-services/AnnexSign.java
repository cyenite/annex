import com.android.apksig.ApkSigner;

import java.io.File;
import java.io.FileInputStream;
import java.security.KeyStore;
import java.security.PrivateKey;
import java.security.cert.X509Certificate;
import java.util.List;

// Signs a stand-in APK with this machine's key: AnnexSign <keystore> <password> <in> <out>.
// apksig has no command line of its own, and the JRE annex builds in has no jarsigner.
public final class AnnexSign {
    public static void main(String[] args) throws Exception {
        char[] password = args[1].toCharArray();
        KeyStore store = KeyStore.getInstance("PKCS12");
        try (FileInputStream in = new FileInputStream(args[0])) {
            store.load(in, password);
        }
        String alias = store.aliases().nextElement();
        PrivateKey key = (PrivateKey) store.getKey(alias, password);
        X509Certificate certificate = (X509Certificate) store.getCertificate(alias);
        ApkSigner.SignerConfig signer =
                new ApkSigner.SignerConfig.Builder("ANNEX", key, List.of(certificate)).build();
        new ApkSigner.Builder(List.of(signer))
                .setInputApk(new File(args[2]))
                .setOutputApk(new File(args[3]))
                .setV1SigningEnabled(true)
                .setV2SigningEnabled(true)
                .setV3SigningEnabled(true)
                .build()
                .sign();
    }
}
