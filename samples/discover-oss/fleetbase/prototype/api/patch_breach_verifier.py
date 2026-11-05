#!/usr/bin/env python3
"""Make the pinned Laravel breach verifier fail closed when its service is unavailable."""
from hashlib import sha256
from pathlib import Path
p=Path(__file__).parent
source=(p/'NotPwnedVerifier.php').read_bytes()
if sha256(source).hexdigest()!='de0f50bf6033ad5a13385d9eceacc5c17de9c3c68bcbc591cefd08c379a7bb10':
    raise ValueError('unexpected upstream Laravel verifier')
old=b'''        } catch (Exception $e) {
            report($e);
        }

        $body = (isset($response) && $response->successful())
            ? $response->body()
            : '';'''
new=b'''        } catch (Exception $e) {
            // Do not log the password hash prefix from an HTTP exception.
            throw new \\Symfony\\Component\\HttpKernel\\Exception\\ServiceUnavailableHttpException(
                60, 'Password safety check is unavailable. Please try again later.'
            );
        }

        if (! $response->successful()) {
            throw new \\Symfony\\Component\\HttpKernel\\Exception\\ServiceUnavailableHttpException(
                60, 'Password safety check is unavailable. Please try again later.'
            );
        }

        $body = $response->body();'''
if source.count(old)!=1:raise ValueError('unexpected verifier code')
patched = source.replace(old, new)
remote = b'https://api.pwnedpasswords.com/range/'
if patched.count(remote) != 1:
    raise ValueError('unexpected breach-check endpoint')
patched = patched.replace(remote, b'http://breachcheck:8081/range/')
(p/'NotPwnedVerifier-ato.php').write_bytes(patched)
print('Pinned verifier patched to fail closed without logging hash prefixes.')
