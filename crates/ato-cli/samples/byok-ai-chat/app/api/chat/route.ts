import { createOpenAI } from '@ai-sdk/openai';
import { streamText, convertToCoreMessages } from 'ai';

// Edge runtime for optimal performance (Vercel deployment)
// Comment out for local development if needed
export const runtime = 'edge';

export async function POST(req: Request) {
  const { messages, apiKey, baseUrl } = await req.json();

  // Hybrid Auth: Check environment variable first (Vault integration)
  // Falls back to request body apiKey (UI input via localStorage)
  const resolvedApiKey = process.env.OPENAI_API_KEY || apiKey;
  const resolvedBaseUrl = process.env.OPENAI_BASE_URL || baseUrl;

  if (!resolvedApiKey) {
    return new Response(
      JSON.stringify({
        error: 'API Key is required',
        code: 'MISSING_API_KEY',
        hint: 'Set OPENAI_API_KEY environment variable or provide apiKey in request',
      }),
      {
        status: 401,
        headers: { 'Content-Type': 'application/json' },
      }
    );
  }

  try {
    // Create OpenAI client with resolved credentials
    const openai = createOpenAI({
      apiKey: resolvedApiKey,
      ...(resolvedBaseUrl && { baseURL: resolvedBaseUrl }),
    });

    const result = streamText({
      model: openai('gpt-4o-mini'), // Default to cost-effective model
      messages: convertToCoreMessages(messages),
    });

    return result.toDataStreamResponse();
  } catch (error) {
    console.error('Chat API error:', error);
    
    const errorMessage = error instanceof Error ? error.message : 'Unknown error';
    const isAuthError = errorMessage.includes('401') || errorMessage.includes('Invalid API Key');
    
    return new Response(
      JSON.stringify({
        error: isAuthError ? 'Invalid API Key' : 'Chat request failed',
        code: isAuthError ? 'INVALID_API_KEY' : 'CHAT_ERROR',
        details: errorMessage,
      }),
      {
        status: isAuthError ? 401 : 500,
        headers: { 'Content-Type': 'application/json' },
      }
    );
  }
}
