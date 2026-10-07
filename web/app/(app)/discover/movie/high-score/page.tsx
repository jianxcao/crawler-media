import { Navigate } from "react-router-dom";

/** 兼容旧书签；片单页面已统一使用稳定 collectionRef 路由。 */
export default function HighScorePage() {
  return <Navigate to="/discover/movie/collections/douban/top-rated" replace />;
}
