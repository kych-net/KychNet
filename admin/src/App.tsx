import { useEffect, useState } from "react";
import { api, User } from "./api";
import { AuthPage } from "./pages/AuthPage";
import { Dashboard } from "./pages/Dashboard";

export function App() {
  const [user, setUser] = useState<User | null>(null);
  const [loading, setLoading] = useState(true);

  useEffect(() => {
    api
      .me()
      .then(setUser)
      .catch(() => setUser(null))
      .finally(() => setLoading(false));
  }, []);

  if (loading) {
    return <p className="hint">加载中…</p>;
  }
  if (!user) {
    return <AuthPage onLogin={setUser} />;
  }
  return <Dashboard user={user} onLogout={() => setUser(null)} />;
}
