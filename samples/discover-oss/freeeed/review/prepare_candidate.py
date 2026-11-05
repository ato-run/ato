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
    destination.mkdir(parents=True)
    for name in ('pom.xml', 'LICENSE', 'README.md'):
        shutil.copy2(source / name, destination / name)
    shutil.copytree(source / 'src', destination / 'src', symlinks=False)
    (destination / 'src/main/resources/paContext.xml').write_text(xml)
    (destination / relative_case).write_text(text)
    print('Local candidate prepared: per-request controllers and protected internal case creation.')
    print('Not deployable: default admin credentials, plaintext user storage and other security review remain.')


if __name__ == '__main__':
    main()
