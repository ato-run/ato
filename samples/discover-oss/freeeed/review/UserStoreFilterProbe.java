package org.freeeed.search.web.dao.user;

import java.io.*;
import java.util.HashMap;
import java.util.Map;
import org.freeeed.search.web.model.User;

/** Synthetic regression probe; the rejected object's deserializer must never run. */
public final class UserStoreFilterProbe {
    private static boolean executed;
    private static final class Forbidden implements Serializable {
        private static final long serialVersionUID = 1L;
        private void readObject(ObjectInputStream input) throws IOException, ClassNotFoundException {
            executed = true;
            input.defaultReadObject();
        }
    }
    private static byte[] serialize(Object value) throws IOException {
        ByteArrayOutputStream bytes = new ByteArrayOutputStream();
        try (ObjectOutputStream output = new ObjectOutputStream(bytes)) { output.writeObject(value); }
        return bytes.toByteArray();
    }
    private static Object read(byte[] bytes) throws Exception {
        try (ObjectInputStream input = new ObjectInputStream(new ByteArrayInputStream(bytes))) {
            input.setObjectInputFilter(new UserStoreFilter());
            return input.readObject();
        }
    }
    public static void main(String[] args) throws Exception {
        User user = new User();
        user.setUsername("synthetic");
        user.setId(1L);
        user.addRight(User.Right.CASES);
        Map<String, Object> users = new HashMap<>();
        users.put("synthetic", user);
        if (!(read(serialize(users)) instanceof HashMap)) throw new AssertionError("valid store rejected");
        users.put("synthetic", new Forbidden());
        try {
            read(serialize(users));
            throw new AssertionError("unexpected class accepted");
        } catch (InvalidClassException expected) {
            if (executed) throw new AssertionError("forbidden deserializer ran");
        }
        try {
            read(serialize(new byte[10001]));
            throw new AssertionError("oversized array accepted");
        } catch (InvalidClassException expected) { }
        System.out.println("Valid store accepted; forbidden deserializer and oversized array rejected");
    }
}
