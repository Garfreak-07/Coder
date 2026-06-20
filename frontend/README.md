# Coder Frontend

React + TypeScript + Vite frontend for the Planner-led local agent workflow
workbench.

## Run locally

Start the existing API:

```powershell
coder-api --host 127.0.0.1 --port 8876
```

Install frontend dependencies and start Vite:

```powershell
cd frontend
npm install
npm run dev
```

Open:

```text
http://127.0.0.1:5173
```

Vite proxies `/api/*` to the local FastAPI backend on port `8876`.

## Serve the built frontend from the API

After building:

```powershell
npm run build
cd ..
coder-api --host 127.0.0.1 --port 8876
```

If `frontend/dist` exists, the API serves it at:

```text
http://127.0.0.1:8876
```

You can override the directory explicitly:

```powershell
coder-api --frontend-dist F:\bbb\coder\frontend\dist
```
