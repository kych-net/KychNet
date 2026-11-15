//! 全量 clone 的 pack 结果缓存。
//!
//! 站点每次 clone 都要遍历对象图、解压、重新压缩并算 SHA-1，同一个 ref
//! 的结果完全可以复用。这里只缓存「无 have / 无 shallow / 无 filter 的
//! 全量 clone」——增量 fetch 的 want/have 组合太多，缓存命中率极低，
//! 仍然走实时组装。ref 一变就整体失效，见 `store::set_ref` / `delete_ref`。

use axum::body::Bytes;
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, OnceLock};
use uuid::Uuid;

use super::object::Oid;
use super::proto::UploadRequest;

/// 缓存总字节上限。超过就按插入顺序淘汰最旧的 pack。
const MAX_BYTES: usize = 64 * 1024 * 1024;

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct CacheKey {
    pub project_id: Uuid,
    pub wants: Vec<Oid>,
}

/// 只有「干净的全量 clone」才进缓存：协商、浅克隆、加深与 blob 过滤都会
/// 让同一组 want 对应不同的对象集合，混用会发出错误的 pack。
pub fn key_for(project_id: Uuid, request: &UploadRequest) -> Option<CacheKey> {
    if !request.done
        || request.wants.is_empty()
        || !request.haves.is_empty()
        || !request.shallow.is_empty()
        || request.deepen.is_some()
        || request.deepen_relative
        || request.filter_blob_none
    {
        return None;
    }
    let mut wants = request.wants.clone();
    wants.sort();
    wants.dedup();
    Some(CacheKey { project_id, wants })
}

struct Cache {
    entries: HashMap<CacheKey, Arc<Bytes>>,
    order: VecDeque<CacheKey>,
    bytes: usize,
}

impl Cache {
    fn new() -> Self {
        Self {
            entries: HashMap::new(),
            order: VecDeque::new(),
            bytes: 0,
        }
    }

    fn insert(&mut self, key: CacheKey, value: Arc<Bytes>, limit: usize) {
        if value.len() > limit {
            return;
        }
        if let Some(old) = self.entries.remove(&key) {
            self.bytes = self.bytes.saturating_sub(old.len());
            self.order.retain(|item| item != &key);
        }
        while self.bytes + value.len() > limit {
            let Some(evicted) = self.order.pop_front() else {
                break;
            };
            if let Some(old) = self.entries.remove(&evicted) {
                self.bytes = self.bytes.saturating_sub(old.len());
            }
        }
        self.bytes += value.len();
        self.order.push_back(key.clone());
        self.entries.insert(key, value);
    }

    fn invalidate_project(&mut self, project_id: Uuid) {
        let keys: Vec<CacheKey> = self
            .entries
            .keys()
            .filter(|key| key.project_id == project_id)
            .cloned()
            .collect();
        for key in keys {
            if let Some(old) = self.entries.remove(&key) {
                self.bytes = self.bytes.saturating_sub(old.len());
            }
        }
        self.order.retain(|key| key.project_id != project_id);
    }
}

fn global() -> &'static Mutex<Cache> {
    static CACHE: OnceLock<Mutex<Cache>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(Cache::new()))
}

pub fn get(key: &CacheKey) -> Option<Arc<Bytes>> {
    global().lock().ok()?.entries.get(key).cloned()
}

pub fn insert(key: CacheKey, value: Arc<Bytes>) {
    if let Ok(mut cache) = global().lock() {
        cache.insert(key, value, MAX_BYTES);
    }
}

/// 该项目的任何 ref 变更后调用，清掉所有相关 pack。
pub fn invalidate_project(project_id: Uuid) {
    if let Ok(mut cache) = global().lock() {
        cache.invalidate_project(project_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(project_id: Uuid, n: u8) -> CacheKey {
        CacheKey {
            project_id,
            wants: vec![Oid::from_bytes([n; 20])],
        }
    }

    #[test]
    fn insert_get_and_invalidate() {
        let mut cache = Cache::new();
        let project = Uuid::new_v4();
        let other = Uuid::new_v4();
        let a = key(project, 1);
        let b = key(other, 2);
        let payload = Arc::new(Bytes::from(vec![7u8; 100]));
        cache.insert(a.clone(), payload.clone(), 1000);
        cache.insert(b.clone(), Arc::new(Bytes::from(vec![8u8; 100])), 1000);
        assert!(Arc::ptr_eq(cache.entries.get(&a).unwrap(), &payload));
        assert_eq!(cache.bytes, 200);

        cache.invalidate_project(project);
        assert!(!cache.entries.contains_key(&a));
        assert!(cache.entries.contains_key(&b));
        assert_eq!(cache.bytes, 100);
        assert_eq!(cache.order.len(), 1);
    }

    #[test]
    fn evicts_oldest_when_over_limit() {
        let mut cache = Cache::new();
        let project = Uuid::new_v4();
        let first = key(project, 1);
        let second = key(project, 2);
        cache.insert(first.clone(), Arc::new(Bytes::from(vec![1u8; 60])), 100);
        cache.insert(second.clone(), Arc::new(Bytes::from(vec![2u8; 60])), 100);
        assert!(!cache.entries.contains_key(&first));
        assert!(cache.entries.contains_key(&second));
        assert_eq!(cache.bytes, 60);
        // 覆盖同 key 不能重复计字节。
        cache.insert(second.clone(), Arc::new(Bytes::from(vec![3u8; 30])), 100);
        assert_eq!(cache.bytes, 30);
        assert_eq!(cache.order.len(), 1);
    }

    #[test]
    fn key_for_only_accepts_clean_full_clone() {
        let project = Uuid::new_v4();
        let mut request = UploadRequest::default();
        request.wants.push(Oid::zero());
        request.done = true;
        assert!(key_for(project, &request).is_some());

        request.haves.push(Oid::zero());
        assert!(key_for(project, &request).is_none());
        request.haves.clear();
        request.filter_blob_none = true;
        assert!(key_for(project, &request).is_none());
        request.filter_blob_none = false;
        request.done = false;
        assert!(key_for(project, &request).is_none());
    }
}

/// 同一 key 的「进行中组包」锁。
///
/// 多个客户端同时 clone 同一个项目时，缓存都是未命中，会各自完整组包
/// 一遍（遍历对象图 + 解压 + 重新压缩 + 算 SHA-1）。这里让同一个 key
/// 的组包串行化，后来者在拿到锁后先复查缓存，命中就直接复用。
fn inflight() -> &'static Mutex<HashMap<CacheKey, Arc<tokio::sync::Mutex<()>>>> {
    static INFLIGHT: OnceLock<Mutex<HashMap<CacheKey, Arc<tokio::sync::Mutex<()>>>>> =
        OnceLock::new();
    INFLIGHT.get_or_init(|| Mutex::new(HashMap::new()))
}

/// 取（或创建）某个 key 的组包锁。
pub fn inflight_lock(key: &CacheKey) -> Arc<tokio::sync::Mutex<()>> {
    let mut map = inflight().lock().unwrap_or_else(|e| e.into_inner());
    map.entry(key.clone())
        .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
        .clone()
}

/// ref 变化或项目删除时顺带把锁表里的条目清掉，避免无限增长。
pub fn forget_inflight(project_id: Uuid) {
    let mut map = inflight().lock().unwrap_or_else(|e| e.into_inner());
    map.retain(|key, _| key.project_id != project_id);
}

/// 组包结束后回收该 key 的锁（若已无人等待）。
///
/// 锁表按 wants 组合建 key，同一 ref 下不同 wants 的 clone 会不断留下条目，
/// 而且它不受 pack 缓存 64 MiB 上限约束，所以必须用完即删。
/// 引用计数：锁表 1 + 调用方 `lock` 1 + 仍在等待的 guard 各 1，
/// 因此 ≤ 3 就代表没有其他等待者。
pub fn release_inflight(key: &CacheKey, lock: &Arc<tokio::sync::Mutex<()>>) {
    let mut map = inflight().lock().unwrap_or_else(|e| e.into_inner());
    let Some(entry) = map.get(key) else {
        return;
    };
    if !Arc::ptr_eq(entry, lock) {
        return;
    }
    if Arc::strong_count(entry) <= 3 {
        map.remove(key);
    }
}
