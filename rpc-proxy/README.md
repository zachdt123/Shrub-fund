# Shrub Fund RPC Proxy

Secure proxy service to protect RPC API keys from frontend exposure.

## Setup

1. Install dependencies:
```bash
npm install
```

2. Create `.env` file:
```bash
cp .env.example .env
# Edit .env with your API keys
```

3. Get new Helius API keys:
- Go to https://helius.xyz/
- Create 2-3 new API keys for redundancy
- Add them to `.env`

4. Start the service:
```bash
npm start
# or for development:
npm run dev
```

## Usage

Update your frontend to use the proxy:

```javascript
// OLD (exposed API key):
const endpoint = 'https://mainnet.helius-rpc.com/?api-key=exposed-key';

// NEW (secure proxy):
const endpoint = 'http://localhost:3001/rpc'; // or your deployed URL
```

## Deployment

### Option 1: Same server as frontend
- Deploy alongside your React app
- Use reverse proxy (nginx) to route `/rpc` to this service

### Option 2: Separate service
- Deploy to Vercel/Railway/Heroku
- Update CORS settings for your domain
- Update frontend to use deployed proxy URL

## Security Features

- ✅ Rate limiting (100 requests/minute per IP)
- ✅ Method whitelisting (only safe RPC methods)
- ✅ CORS protection
- ✅ Request validation  
- ✅ Multiple RPC fallbacks
- ✅ Error handling (no internal details exposed)
- ✅ Helmet security headers

## Monitoring

Check service health:
```bash
curl http://localhost:3001/health
```

Logs show:
- Request methods and RPC URLs used
- Fallback URL switching
- Rate limiting hits
- Error details (server-side only)