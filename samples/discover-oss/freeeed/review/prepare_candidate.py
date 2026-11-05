#!/usr/bin/env python3
"""Prepare a local-only FreeEedUI candidate; never run it with real evidence."""
import argparse
import hashlib
from pathlib import Path
import re
import shutil

FILES = {
    'src/main/java/org/freeeed/search/web/controller/CaseController.java':
        '3afb6847680ec84f1d052df961136cbacb074c83e21376b34efc75e30c0df936',
    'src/main/resources/paContext.xml':
        '6b9001863613c5c6bcabe72fdea53624a3e2c4a14a787f61551e8065981086bb',
    'src/main/java/org/freeeed/search/web/dao/user/FSUserDao.java':
        '1e1e87f6e74f65c2597767ed46b2a13c0be1f46beda4328eb93080389e7a9ed1',
    'src/main/webapp/template/left.jsp':
        '5f101bc0a924d269d497a6d32c030c02f116b214eafa357589ebecf3e54ef7c7',
}


def replace_once(text, old, new):
    if text.count(old) != 1:
        raise ValueError('Unexpected upstream source structure')
    return text.replace(old, new, 1)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('source', type=Path)
    parser.add_argument('destination', type=Path)
    args = parser.parse_args()
    source = args.source.resolve(strict=True)
    destination = args.destination.resolve()
    if destination.exists() or destination.is_relative_to(source):
        raise ValueError('Destination must be a new directory outside the source')
    for name, expected in FILES.items():
        if hashlib.sha256((source / name).read_bytes()).hexdigest() != expected:
            raise ValueError('Unexpected upstream file: ' + name)
    if any(entry.is_symlink() for entry in (source / 'src').rglob('*')):
        raise ValueError('Source tree may not contain symlinks')
    xml = (source / 'src/main/resources/paContext.xml').read_text()
    xml, count = re.subn(
        r'(<bean\s+id="[^"]+"\s+class="org\.freeeed\.search\.web\.controller\.[^"]+")>',
        r'\1 scope="prototype">', xml,
    )
    if count != 14:
        raise ValueError('Unexpected controller bean inventory')
    relative_case = 'src/main/java/org/freeeed/search/web/controller/CaseController.java'
    text = (source / relative_case).read_text()
    text = replace_once(text, 'import java.net.URL;',
        'import java.net.URL;\nimport java.nio.charset.StandardCharsets;\nimport java.security.MessageDigest;')
    old = '''        // case creation also remotely identified by a remoteCreation
        String remoteCreation = (String) valueStack.get("remotecasecreation");
        Boolean remoteCaseCreation = remoteCreation != null && remoteCreation.equals("yes");
        if (!remoteCaseCreation
                && !loggedSiteVisitor.getUser().hasRight(User.Right.CASES)) {
            try {
                response.sendRedirect(WebConstants.MAIN_PAGE_REDIRECT);
                return new ModelAndView(WebConstants.CASE_PAGE);
            } catch (IOException e) {
            }
        }
'''
    new = '''        boolean remoteRequested = "yes".equals(valueStack.get("remotecasecreation"));
        boolean remoteCaseCreation = remoteRequested && trustedInternalCaseCreation();
        if ((remoteRequested && !remoteCaseCreation) || (!remoteCaseCreation
                && (loggedSiteVisitor == null || loggedSiteVisitor.getUser() == null
                    || !loggedSiteVisitor.getUser().hasRight(User.Right.CASES)))) {
            try {
                response.sendError(403);
            } catch (IOException ignored) {
                log.warn("Unable to send case authorization refusal");
            }
            return null;
        }
'''
    text = replace_once(text, old, new)
    marker = '    @Override\n    public ModelAndView execute() {'
    helper = '''    // The engine and review share one private network namespace. A browser
    // request forwarded over loopback still needs the per-instance secret.
    private boolean trustedInternalCaseCreation() {
        String expected = System.getenv("FREEEED_INTERNAL_CASE_TOKEN");
        String supplied = request.getHeader("X-FreeEed-Internal-Token");
        String peer = request.getRemoteAddr();
        return expected != null && expected.matches("[A-Za-z0-9_-]{32,128}")
                && supplied != null && supplied.length() <= 128
                && ("127.0.0.1".equals(peer) || "::1".equals(peer)
                    || "0:0:0:0:0:0:0:1".equals(peer))
                && "POST".equals(request.getMethod())
                && "save".equals(valueStack.get("action"))
                && MessageDigest.isEqual(expected.getBytes(StandardCharsets.UTF_8),
                                         supplied.getBytes(StandardCharsets.UTF_8));
    }

'''
    text = replace_once(text, marker, helper + marker)

    user_path = 'src/main/java/org/freeeed/search/web/dao/user/FSUserDao.java'
    users = (source / user_path).read_text()
    users = replace_once(users, 'import java.util.Map;',
        '''import java.util.Map;
import java.util.Objects;
import java.util.EnumSet;
import java.nio.file.Files;
import java.nio.file.LinkOption;
import java.nio.file.Path;
import java.nio.file.Paths;
import java.nio.file.StandardCopyOption;
import java.nio.file.attribute.PosixFilePermission;''')
    users = replace_once(users, '        loadUsers();\n        createAdminUser();',
        '''        loadUsers();
        for (User existing : userCache.values()) {
            if (existing == null || !PasswordVerifier.isEncoded(existing.getPassword())) {
                throw new IllegalStateException("Legacy credentials require an explicit migration");
            }
        }
        createAdminUser();''')
    users = replace_once(users, '''            userCache.put(user.getUsername(), user);
            storeUsers();''', '''            User previous = userCache.get(user.getUsername());
            if (previous == null || !Objects.equals(previous.getPassword(), user.getPassword())) {
                user.setPassword(PasswordVerifier.hash(user.getPassword()));
            } else if (!PasswordVerifier.isEncoded(user.getPassword())) {
                throw new IllegalStateException("Legacy credentials require an explicit migration");
            }
            userCache.put(user.getUsername(), user);
            try {
                storeUsers();
            } catch (RuntimeException error) {
                if (previous == null) userCache.remove(user.getUsername());
                else userCache.put(user.getUsername(), previous);
                throw error;
            }''')
    users = replace_once(users, '''            userCache.remove(username);
            storeUsers();''', '''            User previous = userCache.remove(username);
            try {
                storeUsers();
            } catch (RuntimeException error) {
                if (previous != null) userCache.put(username, previous);
                throw error;
            }''')
    users = replace_once(users, '''                if (password != null) {
                    if (password.equals(user.getPassword())) {
                        return user;
                    }
                }''', '''                if (PasswordVerifier.verify(password, user.getPassword())) {
                    return user;
                }''')
    start = users.index('    private void storeUsers() {')
    end = users.index('    private void loadUsers() {', start)
    users = users[:start] + '''    private void storeUsers() {
        Path directory = Paths.get("work");
        Path target = directory.resolve("u.dat");
        Path temporary = null;
        try {
            if (Files.isSymbolicLink(directory) || Files.isSymbolicLink(target)) {
                throw new IOException("Invalid user store path");
            }
            Files.createDirectories(directory);
            temporary = Files.createTempFile(directory, ".users-", ".new");
            Files.setPosixFilePermissions(temporary, EnumSet.of(
                    PosixFilePermission.OWNER_READ, PosixFilePermission.OWNER_WRITE));
            try (FileOutputStream file = new FileOutputStream(temporary.toFile());
                 ObjectOutputStream output = new ObjectOutputStream(file)) {
                output.writeObject(userCache);
                output.flush();
                file.getFD().sync();
            }
            Files.move(temporary, target, StandardCopyOption.ATOMIC_MOVE,
                       StandardCopyOption.REPLACE_EXISTING);
        } catch (IOException error) {
            throw new IllegalStateException("Unable to persist user store", error);
        } finally {
            if (temporary != null) {
                try { Files.deleteIfExists(temporary); }
                catch (IOException error) { log.warn("Unable to remove temporary user store"); }
            }
        }
    }

''' + users[end:]
    users = replace_once(users, '''        try {
            fis = new FileInputStream(USERS_FILE);''', '''        try {
            Path directory = Paths.get("work");
            Path store = Paths.get(USERS_FILE);
            if (Files.isSymbolicLink(directory) || Files.isSymbolicLink(store)) {
                throw new IOException("Invalid user store path");
            }
            fis = new FileInputStream(USERS_FILE);''')
    users = replace_once(users, '''        } catch (java.io.FileNotFoundException e) {
            // First run: no users have been saved yet. Not an error -- the cache
            // starts empty and the file is created on the first save. (Logged loudly
            // as ERROR before, which made a normal fresh install look broken.)
            log.info("No stored users yet (" + USERS_FILE + "); starting with an empty user cache.");
        } catch (Exception e) {
            log.error("Problem loading users from file system!", e);''', '''        } catch (java.io.FileNotFoundException e) {
            if (Files.exists(Paths.get(USERS_FILE), LinkOption.NOFOLLOW_LINKS)) {
                throw new IllegalStateException("User store is not readable", e);
            }
            log.info("No stored users yet; starting with an empty user cache.");
        } catch (Exception e) {
            throw new IllegalStateException("Unable to load user store", e);''')
    users = replace_once(users, '''            if (data != null) {
                userCache = data;
            }''', '''            if (data == null) throw new IllegalStateException("User store is empty or invalid");
            userCache = data;''')
    users = replace_once(users, '''        if (userCache.containsKey("admin")) {
            return;
        }
\x20\x20\x20\x20\x20\x20\x20\x20
        User adminUser = new User();''', '''        if (userCache.containsKey("admin")) {
            return;
        }
        if (Files.exists(Paths.get(USERS_FILE), LinkOption.NOFOLLOW_LINKS)) {
            throw new IllegalStateException("Existing user store has no administrator");
        }

        User adminUser = new User();''')
    users = replace_once(users, '        adminUser.setPassword("admin");',
        '''        String initialPassword = System.getenv("ATO_BINDING_INITIAL_ADMIN_PASSWORD");
        adminUser.setPassword(PasswordVerifier.hash(initialPassword));''')
    users = replace_once(users, '        userCache.put(adminUser.getUsername(), adminUser);',
        '''        userCache.put(adminUser.getUsername(), adminUser);
        try {
            storeUsers();
        } catch (RuntimeException error) {
            userCache.remove(adminUser.getUsername());
            throw error;
        }''')
    left_path = 'src/main/webapp/template/left.jsp'
    left = replace_once((source / left_path).read_text(),
        '<input type="password" class="login-input" value="admin" placeholder="Password"',
        '<input type="password" class="login-input" placeholder="Password"')
    destination.mkdir(parents=True)
    for name in ('pom.xml', 'LICENSE', 'README.md'):
        shutil.copy2(source / name, destination / name)
    shutil.copytree(source / 'src', destination / 'src', symlinks=False)
    (destination / 'src/main/resources/paContext.xml').write_text(xml)
    (destination / relative_case).write_text(text)
    (destination / user_path).write_text(users)
    (destination / left_path).write_text(left)
    shutil.copyfile(Path(__file__).with_name('PasswordVerifier.java'),
        destination / 'src/main/java/org/freeeed/search/web/dao/user/PasswordVerifier.java')
    print('Local candidate prepared: protected case creation and bound, hashed credentials.')
    print('Not deployable: data migration, recovery, rate limits and other security review remain.')


if __name__ == '__main__':
    main()
