package org.freeeed.search.web.dao.user;

import java.security.GeneralSecurityException;
import java.security.MessageDigest;
import java.security.SecureRandom;
import java.util.Base64;
import javax.crypto.SecretKeyFactory;
import javax.crypto.spec.PBEKeySpec;

/** Local prototype: versioned salted PBKDF2 password storage. */
final class PasswordVerifier {
    private static final String PREFIX = "pbkdf2-sha256";
    private static final int ITERATIONS = 600_000;
    private static final int SALT_BYTES = 16;
    private static final int HASH_BYTES = 32;
    private static final SecureRandom RANDOM = new SecureRandom();

    private PasswordVerifier() {}

    static String hash(String password) {
        if (password == null || password.length() < 16 || password.length() > 1024) {
            throw new IllegalArgumentException("Password length must be between 16 and 1024 characters");
        }
        byte[] salt = new byte[SALT_BYTES];
        RANDOM.nextBytes(salt);
        byte[] derived = derive(password, salt);
        Base64.Encoder encoder = Base64.getUrlEncoder().withoutPadding();
        return PREFIX + "$" + ITERATIONS + "$" + encoder.encodeToString(salt)
                + "$" + encoder.encodeToString(derived);
    }

    static boolean isEncoded(String encoded) {
        return parse(encoded) != null;
    }

    static boolean verify(String password, String encoded) {
        if (password == null || password.length() < 16 || password.length() > 1024) return false;
        byte[][] parts = parse(encoded);
        if (parts == null) return false;
        return MessageDigest.isEqual(parts[1], derive(password, parts[0]));
    }

    private static byte[][] parse(String encoded) {
        if (encoded == null || encoded.length() > 160) return null;
        String[] parts = encoded.split("\\$", -1);
        if (parts.length != 4 || !PREFIX.equals(parts[0])
                || !Integer.toString(ITERATIONS).equals(parts[1])) return null;
        try {
            Base64.Decoder decoder = Base64.getUrlDecoder();
            byte[] salt = decoder.decode(parts[2]);
            byte[] hash = decoder.decode(parts[3]);
            Base64.Encoder encoder = Base64.getUrlEncoder().withoutPadding();
            if (salt.length != SALT_BYTES || hash.length != HASH_BYTES
                    || !encoder.encodeToString(salt).equals(parts[2])
                    || !encoder.encodeToString(hash).equals(parts[3])) return null;
            return new byte[][] {salt, hash};
        } catch (IllegalArgumentException invalid) {
            return null;
        }
    }

    private static byte[] derive(String password, byte[] salt) {
        PBEKeySpec spec = new PBEKeySpec(password.toCharArray(), salt, ITERATIONS, HASH_BYTES * 8);
        try {
            return SecretKeyFactory.getInstance("PBKDF2WithHmacSHA256").generateSecret(spec).getEncoded();
        } catch (GeneralSecurityException error) {
            throw new IllegalStateException("Password verifier is unavailable", error);
        } finally {
            spec.clearPassword();
        }
    }
}
