package org.freeeed.search.web.dao.user;

import java.security.SecureRandom;
import java.util.Base64;

/** Uses only random synthetic values; run outside any user instance. */
public final class PasswordVerifierProbe {
    public static void main(String[] args) {
        byte[] data = new byte[32];
        new SecureRandom().nextBytes(data);
        String password = Base64.getUrlEncoder().withoutPadding().encodeToString(data);
        String first = PasswordVerifier.hash(password);
        String second = PasswordVerifier.hash(password);
        if (first.equals(second) || !PasswordVerifier.isEncoded(first)
                || !PasswordVerifier.verify(password, first)
                || PasswordVerifier.verify(password + "x", first)
                || PasswordVerifier.verify(password, first.replace("$600000$", "$999999999$"))
                || PasswordVerifier.verify(password, first + "=")
                || PasswordVerifier.verify(password, "legacy-plaintext")) {
            throw new AssertionError("password verifier check failed");
        }
        try {
            PasswordVerifier.hash("short");
            throw new AssertionError("short password accepted");
        } catch (IllegalArgumentException expected) {
            System.out.println("Random salts, verification, malformed hash, and length checks passed");
        }
    }
}
