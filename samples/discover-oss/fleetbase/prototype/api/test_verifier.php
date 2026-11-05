<?php
// Run inside the prototype image from /fleetbase/api; no network is used.
require 'vendor/autoload.php';
use Illuminate\Http\Client\Factory;
use Illuminate\Validation\NotPwnedVerifier;
use Symfony\Component\HttpKernel\Exception\ServiceUnavailableHttpException;

$password = 'Synthetic-Local-Password-2!';
$suffix = substr(strtoupper(sha1($password)), 5);
function verifyUsing($callback, $password) {
    $factory = new Factory();
    $factory->fake($callback);
    return (new NotPwnedVerifier($factory))->verify(['value' => $password, 'threshold' => 0]);
}
$compromised = verifyUsing(fn () => Factory::response($suffix . ":7\n", 200), $password) === false;
$clean = verifyUsing(fn () => Factory::response(str_repeat('A', 35) . ":1\n", 200), $password) === true;
try {
    verifyUsing(fn () => Factory::response('', 503), $password);
    $httpUnavailable = false;
} catch (ServiceUnavailableHttpException $e) {
    $httpUnavailable = $e->getStatusCode() === 503;
}
try {
    verifyUsing(fn () => throw new RuntimeException('network unavailable'), $password);
    $networkUnavailable = false;
} catch (ServiceUnavailableHttpException $e) {
    $networkUnavailable = $e->getStatusCode() === 503;
}
if (! $compromised || ! $clean || ! $httpUnavailable || ! $networkUnavailable) {
    fwrite(STDERR, "Verifier test failed\n");
    exit(1);
}
echo "Compromised, clean, HTTP failure, and network failure passed\n";
