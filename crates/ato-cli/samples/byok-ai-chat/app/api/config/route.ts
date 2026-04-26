import { NextResponse } from 'next/server';

/**
 * Configuration endpoint for BYOK status detection
 * 
 * Security: Returns ONLY boolean flags, NEVER actual key values
 * 
 * This allows the client to detect whether:
 * - Vault integration is configured (env var present)
 * - Custom base URL is configured
 */
export async function GET() {
  // Check if environment variables are set (Vault integration)
  // SECURITY: Never expose the actual key values
  const hasEnvKey = Boolean(process.env.OPENAI_API_KEY);
  const hasEnvBaseUrl = Boolean(process.env.OPENAI_BASE_URL);

  return NextResponse.json({
    // Auth configuration status
    hasEnvKey,
    hasEnvBaseUrl,
    
    // Auth mode indicator
    authMode: hasEnvKey ? 'vault' : 'byok',
    
    // Feature flags (for future extensibility)
    features: {
      streaming: true,
      multiProvider: true,
    },
  });
}
