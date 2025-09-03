const express = require('express');
const cors = require('cors');
const helmet = require('helmet');
const rateLimit = require('express-rate-limit');
const axios = require('axios');
require('dotenv').config();

const app = express();
const PORT = process.env.PORT || 3001;

// Security middleware
app.use(helmet());
app.use(cors({
  origin: process.env.ALLOWED_ORIGINS?.split(',') || ['http://localhost:3000'],
  credentials: true
}));

// Rate limiting - adjust based on your needs
const limiter = rateLimit({
  windowMs: 1 * 60 * 1000, // 1 minute
  max: 100, // Limit each IP to 100 requests per windowMs
  message: 'Too many RPC requests, please try again later.',
  standardHeaders: true,
  legacyHeaders: false,
});
app.use('/rpc', limiter);

// Body parser
app.use(express.json({ limit: '1mb' }));

// Health check endpoint
app.get('/health', (req, res) => {
  res.json({ status: 'OK', timestamp: new Date().toISOString() });
});

// Main RPC proxy endpoint
app.post('/rpc', async (req, res) => {
  try {
    // Validate request has required fields
    if (!req.body.method) {
      return res.status(400).json({ 
        error: 'Missing required field: method' 
      });
    }

    // Security: Only allow safe RPC methods (add more as needed)
    const allowedMethods = [
      'getAccountInfo',
      'getBalance',
      'getTokenAccountBalance',
      'getTokenAccountsByOwner',
      'getProgramAccounts',
      'getSignaturesForAddress',
      'getTransaction',
      'getParsedTransaction',
      'getConfirmedTransaction',
      'sendTransaction',
      'simulateTransaction',
      'getBlockHeight',
      'getSlot',
      'getLatestBlockhash'
    ];

    if (!allowedMethods.includes(req.body.method)) {
      return res.status(403).json({ 
        error: `Method '${req.body.method}' not allowed` 
      });
    }

    // Get RPC URL from environment (multiple URLs for fallback)
    const rpcUrls = [
      process.env.PRIMARY_RPC_URL,
      process.env.FALLBACK_RPC_URL_1,
      process.env.FALLBACK_RPC_URL_2
    ].filter(Boolean);

    if (rpcUrls.length === 0) {
      throw new Error('No RPC URLs configured');
    }

    let lastError;
    
    // Try each RPC URL until one works
    for (const rpcUrl of rpcUrls) {
      try {
        console.log(`[${new Date().toISOString()}] Proxying ${req.body.method} to ${rpcUrl.split('?')[0]}`);
        
        const response = await axios.post(rpcUrl, req.body, {
          headers: {
            'Content-Type': 'application/json',
          },
          timeout: 30000, // 30 second timeout
        });

        // Forward the response
        return res.json(response.data);
        
      } catch (error) {
        lastError = error;
        console.warn(`RPC URL ${rpcUrl.split('?')[0]} failed:`, error.message);
        continue; // Try next URL
      }
    }

    // All URLs failed
    throw lastError;

  } catch (error) {
    console.error('RPC Proxy Error:', error.message);
    
    // Don't expose internal error details to client
    res.status(500).json({
      error: 'RPC request failed',
      method: req.body.method || 'unknown'
    });
  }
});

// 404 handler
app.use('*', (req, res) => {
  res.status(404).json({ error: 'Endpoint not found' });
});

// Error handler
app.use((error, req, res, next) => {
  console.error('Server Error:', error);
  res.status(500).json({ error: 'Internal server error' });
});

app.listen(PORT, () => {
  console.log(`🚀 RPC Proxy server running on port ${PORT}`);
  console.log(`📡 Health check: http://localhost:${PORT}/health`);
  console.log(`🔗 RPC endpoint: http://localhost:${PORT}/rpc`);
});

module.exports = app;