require 'json'

config = JSON.parse(ENV.fetch('ATO_BINDING_CHATWOOT_RUNTIME'))
required = %w[
  bootstrap_admin_email bootstrap_admin_name bootstrap_admin_password
  bridge_admin_token ato_account_id instance_id oauth_operator_user_id
  oauth_state_key
]
missing = required.reject { |key| config[key].is_a?(String) && !config[key].empty? }
abort("Chatwoot runtime Binding is incomplete") unless missing.empty?

account = Account.find_or_create_by!(name: 'Ato Support Desk')
user = User.find_or_initialize_by(email: config.fetch('bootstrap_admin_email').downcase)
if user.new_record?
  user.name = config.fetch('bootstrap_admin_name')
  user.type = 'SuperAdmin'
  user.password = config.fetch('bootstrap_admin_password')
  user.password_confirmation = config.fetch('bootstrap_admin_password')
  user.confirmed_at = Time.current if user.respond_to?(:confirmed_at=)
  user.save!
end
Redis::Alfred.delete(Redis::Alfred::CHATWOOT_INSTALLATION_ONBOARDING)
AccountUser.find_or_create_by!(account: account, user: user) do |membership|
  membership.role = :administrator
end

inbox = account.inboxes.find_by(name: 'Gmail Support', channel_type: 'Channel::Api')
unless inbox
  channel = Channel::Api.create!(
    account: account,
    webhook_url: 'http://127.0.0.1:8080/__ato/chatwoot/webhook',
    hmac_mandatory: true
  )
  inbox = account.inboxes.create!(name: 'Gmail Support', channel: channel)
end
channel = inbox.channel
channel.update!(
  webhook_url: 'http://127.0.0.1:8080/__ato/chatwoot/webhook',
  hmac_mandatory: true
)
InboxMember.find_or_create_by!(inbox: inbox, user: user)

output = config.slice(
  'bridge_admin_token', 'ato_account_id', 'instance_id',
  'oauth_operator_user_id', 'oauth_state_key',
  'https_proxy_target', 'https_proxy_authorization'
).merge(
  'api_token' => user.access_token.token,
  'account_id' => account.id,
  'inbox_id' => inbox.id,
  'inbox_identifier' => channel.identifier,
  'inbox_hmac_token' => channel.hmac_token,
  'webhook_secret' => channel.secret,
  'allowed_agent_ids' => [user.id]
)
path = '/run/ato-support/chatwoot-runtime.json'
File.open(path, File::WRONLY | File::CREAT | File::TRUNC, 0o600) do |file|
  file.write(JSON.generate(output))
end
