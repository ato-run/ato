require 'net/http'

# Chatwoot correctly blocks private-network webhook destinations by default.
# This Capsule co-locates its signed API Channel receiver in the same container,
# so allow only that immutable loopback destination instead of enabling the
# process-wide SAFE_FETCH_ALLOW_PRIVATE_NETWORK escape hatch.
module AtoLoopbackWebhook
  TARGET = 'http://127.0.0.1:8080/__ato/chatwoot/webhook'.freeze

  private

  def perform_request
    return super unless @webhook_type == :api_inbox_webhook && @url == TARGET

    body = @payload.to_json
    request = Net::HTTP::Post.new('/__ato/chatwoot/webhook', request_headers(body))
    request.body = body
    response = Net::HTTP.start(
      '127.0.0.1', 8080,
      open_timeout: webhook_timeout,
      read_timeout: webhook_timeout
    ) { |http| http.request(request) }
    response.value
  end
end

Rails.application.config.to_prepare do
  Webhooks::Trigger.prepend(AtoLoopbackWebhook) unless Webhooks::Trigger < AtoLoopbackWebhook
end
