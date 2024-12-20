/*
 * Licensed to the Apache Software Foundation (ASF) under one or more
 * contributor license agreements.  See the NOTICE file distributed with
 * this work for additional information regarding copyright ownership.
 * The ASF licenses this file to You under the Apache License, Version 2.0
 * (the "License"); you may not use this file except in compliance with
 * the License.  You may obtain a copy of the License at
 *
 *     http://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 */
use std::collections::HashMap;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use cheetah_string::CheetahString;
use rocketmq_common::common::pop_ack_constants::PopAckConstants;
use rocketmq_common::TimeUtils::get_current_millis;
use rocketmq_remoting::code::request_code::RequestCode;
use rocketmq_remoting::net::channel::Channel;
use rocketmq_remoting::protocol::remoting_command::RemotingCommand;
use rocketmq_remoting::runtime::connection_handler_context::ConnectionHandlerContext;
use rocketmq_store::pop::ack_msg::AckMsg;
use rocketmq_store::pop::batch_ack_msg::BatchAckMsg;
use rocketmq_store::pop::pop_check_point::PopCheckPoint;
use tokio::sync::Mutex;
use tracing::info;

#[derive(Default)]
pub struct PopMessageProcessor {}

impl PopMessageProcessor {
    pub async fn process_request(
        &mut self,
        _channel: Channel,
        _ctx: ConnectionHandlerContext,
        _request_code: RequestCode,
        _request: RemotingCommand,
    ) -> crate::Result<Option<RemotingCommand>> {
        unimplemented!("PopMessageProcessor process_request")
    }

    pub fn queue_lock_manager(&self) -> &QueueLockManager {
        unimplemented!("PopMessageProcessor QueueLockManager")
    }
}

impl PopMessageProcessor {
    pub fn gen_ack_unique_id(ack_msg: &AckMsg) -> String {
        format!(
            "{}{}{}{}{}{}{}{}{}{}{}{}{}",
            ack_msg.topic,
            PopAckConstants::SPLIT,
            ack_msg.queue_id,
            PopAckConstants::SPLIT,
            ack_msg.ack_offset,
            PopAckConstants::SPLIT,
            ack_msg.consumer_group,
            PopAckConstants::SPLIT,
            ack_msg.pop_time,
            PopAckConstants::SPLIT,
            ack_msg.broker_name,
            PopAckConstants::SPLIT,
            PopAckConstants::ACK_TAG
        )
    }

    pub fn gen_batch_ack_unique_id(batch_ack_msg: &BatchAckMsg) -> String {
        format!(
            "{}{}{}{}{:?}{}{}{}{}{}{}",
            batch_ack_msg.ack_msg.topic,
            PopAckConstants::SPLIT,
            batch_ack_msg.ack_msg.queue_id,
            PopAckConstants::SPLIT,
            batch_ack_msg.ack_offset_list,
            PopAckConstants::SPLIT,
            batch_ack_msg.ack_msg.consumer_group,
            PopAckConstants::SPLIT,
            batch_ack_msg.ack_msg.pop_time,
            PopAckConstants::SPLIT,
            PopAckConstants::BATCH_ACK_TAG
        )
    }

    pub fn gen_ck_unique_id(ck: &PopCheckPoint) -> String {
        format!(
            "{}{}{}{}{}{}{}{}{}{}{}{}{}",
            ck.topic,
            PopAckConstants::SPLIT,
            ck.queue_id,
            PopAckConstants::SPLIT,
            ck.start_offset,
            PopAckConstants::SPLIT,
            ck.cid,
            PopAckConstants::SPLIT,
            ck.pop_time,
            PopAckConstants::SPLIT,
            ck.broker_name
                .as_ref()
                .map_or("null".to_string(), |x| x.to_string()),
            PopAckConstants::SPLIT,
            PopAckConstants::CK_TAG
        )
    }
}

struct TimedLock {
    lock: AtomicBool,
    lock_time: AtomicU64,
}

impl TimedLock {
    pub fn new() -> Self {
        TimedLock {
            lock: AtomicBool::new(false),
            lock_time: AtomicU64::new(get_current_millis()),
        }
    }

    pub fn try_lock(&self) -> bool {
        match self
            .lock
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
        {
            Ok(_) => {
                self.lock_time
                    .store(get_current_millis(), Ordering::Relaxed);
                true
            }
            Err(_) => false,
        }
    }

    pub fn unlock(&self) {
        self.lock.store(false, Ordering::Release);
    }

    pub fn is_locked(&self) -> bool {
        self.lock.load(Ordering::Acquire)
    }

    pub fn get_lock_time(&self) -> u64 {
        self.lock_time.load(Ordering::Relaxed)
    }
}

pub struct QueueLockManager {
    expired_local_cache: Arc<Mutex<HashMap<CheetahString, TimedLock>>>,
}

impl QueueLockManager {
    pub fn new() -> Self {
        QueueLockManager {
            expired_local_cache: Arc::new(Mutex::new(HashMap::with_capacity(4096))),
        }
    }

    pub fn build_lock_key(
        topic: &CheetahString,
        consumer_group: &CheetahString,
        queue_id: i32,
    ) -> String {
        format!(
            "{}{}{}{}{}",
            topic,
            PopAckConstants::SPLIT,
            consumer_group,
            PopAckConstants::SPLIT,
            queue_id
        )
    }

    pub async fn try_lock(
        &self,
        topic: &CheetahString,
        consumer_group: &CheetahString,
        queue_id: i32,
    ) -> bool {
        let key = Self::build_lock_key(topic, consumer_group, queue_id);
        self.try_lock_with_key(CheetahString::from_string(key))
            .await
    }

    pub async fn try_lock_with_key(&self, key: CheetahString) -> bool {
        let mut cache = self.expired_local_cache.lock().await;
        let lock = cache.entry(key).or_insert(TimedLock::new());
        lock.try_lock()
    }

    pub async fn unlock(
        &self,
        topic: &CheetahString,
        consumer_group: &CheetahString,
        queue_id: i32,
    ) {
        let key = Self::build_lock_key(topic, consumer_group, queue_id);
        self.unlock_with_key(CheetahString::from_string(key)).await;
    }

    pub async fn unlock_with_key(&self, key: CheetahString) {
        let cache = self.expired_local_cache.lock().await;
        if let Some(lock) = cache.get(&key) {
            lock.unlock();
        }
    }

    pub async fn clean_unused_locks(&self, used_expire_millis: u64) -> usize {
        let mut cache = self.expired_local_cache.lock().await;
        let count = cache.len();
        cache.retain(|_, lock| get_current_millis() - lock.get_lock_time() <= used_expire_millis);
        count
    }

    pub fn start(self: Arc<Self>) {
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(tokio::time::Duration::from_secs(60)).await;
                let count = self.clean_unused_locks(60000).await;
                info!("QueueLockSize={}", count);
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use cheetah_string::CheetahString;

    use super::*;

    #[test]
    fn gen_ack_unique_id_formats_correctly() {
        let ack_msg = AckMsg {
            ack_offset: 123,
            start_offset: 456,
            consumer_group: CheetahString::from_static_str("test_group"),
            topic: CheetahString::from_static_str("test_topic"),
            queue_id: 1,
            pop_time: 789,
            broker_name: CheetahString::from_static_str("test_broker"),
        };
        let result = PopMessageProcessor::gen_ack_unique_id(&ack_msg);
        let expected = "test_topic@1@123@test_group@789@test_broker@ack";
        assert_eq!(result, expected);
    }

    #[test]
    fn gen_batch_ack_unique_id_formats_correctly() {
        let ack_msg = AckMsg {
            ack_offset: 123,
            start_offset: 456,
            consumer_group: CheetahString::from_static_str("test_group"),
            topic: CheetahString::from_static_str("test_topic"),
            queue_id: 1,
            pop_time: 789,
            broker_name: CheetahString::from_static_str("test_broker"),
        };
        let batch_ack_msg = BatchAckMsg {
            ack_msg,
            ack_offset_list: vec![1, 2, 3],
        };
        let result = PopMessageProcessor::gen_batch_ack_unique_id(&batch_ack_msg);
        let expected = "test_topic@1@[1, 2, 3]@test_group@789@bAck";
        assert_eq!(result, expected);
    }

    #[test]
    fn gen_ck_unique_id_formats_correctly() {
        let ck = PopCheckPoint {
            topic: CheetahString::from("test_topic"),
            queue_id: 1,
            start_offset: 456,
            cid: CheetahString::from("test_cid"),
            revive_offset: 0,
            pop_time: 789,
            invisible_time: 0,
            bit_map: 0,
            broker_name: Some(CheetahString::from("test_broker")),
            num: 0,
            queue_offset_diff: vec![],
            re_put_times: None,
        };
        let result = PopMessageProcessor::gen_ck_unique_id(&ck);
        let expected = "test_topic@1@456@test_cid@789@test_broker@ck";
        assert_eq!(result, expected);
    }

    #[test]
    fn new_timed_lock_is_unlocked() {
        let lock = TimedLock::new();
        assert!(!lock.is_locked());
    }

    #[test]
    fn try_lock_locks_successfully() {
        let lock = TimedLock::new();
        assert!(lock.try_lock());
        assert!(lock.is_locked());
    }

    #[test]
    fn try_lock_fails_when_already_locked() {
        let lock = TimedLock::new();
        lock.try_lock();
        assert!(!lock.try_lock());
    }

    #[test]
    fn unlock_unlocks_successfully() {
        let lock = TimedLock::new();
        lock.try_lock();
        lock.unlock();
        assert!(!lock.is_locked());
    }

    #[test]
    fn get_lock_time_returns_correct_time() {
        let lock = TimedLock::new();
        let initial_time = lock.get_lock_time();
        lock.try_lock();
        let lock_time = lock.get_lock_time();
        assert!(lock_time >= initial_time);
    }

    #[tokio::test]
    async fn new_queue_lock_manager_has_empty_cache() {
        let manager = QueueLockManager::new();
        let cache = manager.expired_local_cache.lock().await;
        assert!(cache.is_empty());
    }

    #[tokio::test]
    async fn build_lock_key_formats_correctly() {
        let topic = CheetahString::from_static_str("test_topic");
        let consumer_group = CheetahString::from_static_str("test_group");
        let queue_id = 1;
        let key = QueueLockManager::build_lock_key(&topic, &consumer_group, queue_id);
        let expected = "test_topic@test_group@1";
        assert_eq!(key, expected);
    }

    #[tokio::test]
    async fn try_lock_locks_successfully1() {
        let manager = QueueLockManager::new();
        let topic = CheetahString::from_static_str("test_topic");
        let consumer_group = CheetahString::from_static_str("test_group");
        let queue_id = 1;
        assert!(manager.try_lock(&topic, &consumer_group, queue_id).await);
    }

    #[tokio::test]
    async fn try_lock_fails_when_already_locked1() {
        let manager = QueueLockManager::new();
        let topic = CheetahString::from_static_str("test_topic");
        let consumer_group = CheetahString::from_static_str("test_group");
        let queue_id = 1;
        manager.try_lock(&topic, &consumer_group, queue_id).await;
        assert!(!manager.try_lock(&topic, &consumer_group, queue_id).await);
    }

    #[tokio::test]
    async fn unlock_unlocks_successfully1() {
        let manager = QueueLockManager::new();
        let topic = CheetahString::from_static_str("test_topic");
        let consumer_group = CheetahString::from_static_str("test_group");
        let queue_id = 1;
        manager.try_lock(&topic, &consumer_group, queue_id).await;
        manager.unlock(&topic, &consumer_group, queue_id).await;
        assert!(manager.try_lock(&topic, &consumer_group, queue_id).await);
    }

    #[tokio::test]
    async fn clean_unused_locks_removes_expired_locks() {
        let manager = QueueLockManager::new();
        let topic = CheetahString::from_static_str("test_topic");
        let consumer_group = CheetahString::from_static_str("test_group");
        let queue_id = 1;
        manager.try_lock(&topic, &consumer_group, queue_id).await;
        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
        let removed_count = manager.clean_unused_locks(5).await;
        assert_eq!(removed_count, 1);
        let removed_count = manager.clean_unused_locks(15).await;
        assert_eq!(removed_count, 0);
    }
}
