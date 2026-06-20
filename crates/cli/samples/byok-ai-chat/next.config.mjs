/** @type {import('next').NextConfig} */
const nextConfig = {
  // Standalone output for optimized Capsule distribution (optional)
  // Uncomment for production deployment:
  // output: 'standalone',
  
  // Disable image optimization (not needed for this app)
  images: {
    unoptimized: true,
  },
  
  // Strict mode for better development experience
  reactStrictMode: true,
  
  // Disable telemetry
  telemetry: false,
};

export default nextConfig;
