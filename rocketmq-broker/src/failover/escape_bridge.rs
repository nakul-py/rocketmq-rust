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
use std::sync::Arc;

use cheetah_string::CheetahString;
use rocketmq_client_rust::producer::send_result::SendResult;
use rocketmq_client_rust::producer::send_status::SendStatus;
use rocketmq_common::common::broker::broker_config::BrokerConfig;
use rocketmq_common::common::message::message_ext_broker_inner::MessageExtBrokerInner;
use rocketmq_common::common::message::MessageTrait;
use rocketmq_common::common::mix_all;
use rocketmq_runtime::RocketMQRuntime;
use rocketmq_rust::ArcMut;
use rocketmq_store::base::message_result::PutMessageResult;
use rocketmq_store::base::message_status_enum::PutMessageStatus;
use rocketmq_store::log_file::MessageStore;

use crate::topic::manager::topic_route_info_manager::TopicRouteInfoManager;

const SEND_TIMEOUT: u64 = 3_000;
const DEFAULT_PULL_TIMEOUT_MILLIS: u64 = 10_000;
///### RocketMQ's EscapeBridge for Dead Letter Queue (DLQ) Mechanism
///
/// In the context of message passing within RocketMQ, the `EscapeBridge` primarily handles the Dead
/// Letter Queue (DLQ) mechanism. When messages fail to be successfully consumed by a consumer after
/// multiple attempts, these messages are designated as "dead letters"—messages that cannot be
/// processed normally. To prevent such messages from indefinitely blocking the consumer's
/// processing flow, RocketMQ provides functionality to transfer these messages to a special queue
/// known as the dead letter queue.
///
/// The `EscapeBridge` acts as a bridge in this process, responsible for moving messages that have
/// failed consumption from their original queue into the DLQ. This action helps maintain system
/// health and prevents the entire consumption process from being obstructed by a few problematic
/// messages. Additionally, it provides developers with an opportunity to analyze and address these
/// abnormal messages at a later time.
///
/// #### Functions of EscapeBridge
///
/// - **Isolation of Problematic Messages:** Moves messages that cannot be consumed into the DLQ to
///   ensure they do not continue to affect normal consumption processes.
/// - **Preservation of Message Data:** Even if a message is considered unconsumable, its content is
///   preserved, allowing for subsequent diagnosis or specialized handling.
/// - **Support for Retry Logic:** For messages that may have failed due to transient issues, retry
///   logic can be applied by requeueing or specially processing messages in the DLQ, enabling
///   another attempt at consumption.
///
/// Through this approach, RocketMQ enhances the reliability and stability of the messaging system.
/// It also equips developers with better tools for managing and troubleshooting issues in message
/// transmission.
///
/// #### Conclusion
///
/// RocketMQ's `EscapeBridge` plays a critical role in maintaining the robustness of the messaging
/// system by effectively handling messages that cannot be processed. By isolating problematic
/// messages, preserving their data, and supporting retry mechanisms, it ensures that the overall
/// consumption process remains healthy and unobstructed. Developers gain valuable insights into
/// message failures, aiding in the diagnosis and resolution of potential issues.
///
/// **Note:** The specific configuration and usage methods may vary depending on the version of
/// RocketMQ. Please refer to the official documentation for the most accurate information.
pub(crate) struct EscapeBridge<MS> {
    inner_producer_group_name: CheetahString,
    inner_consumer_group_name: CheetahString,
    escape_bridge_runtime: RocketMQRuntime,
    message_store: ArcMut<MS>,
    broker_config: Arc<BrokerConfig>,
    topic_route_info_manager: Arc<TopicRouteInfoManager>,
}

impl<MS> EscapeBridge<MS>
where
    MS: MessageStore,
{
    pub async fn put_message(
        &mut self,
        mut message_ext: MessageExtBrokerInner,
    ) -> PutMessageResult {
        if self.broker_config.broker_identity.broker_id == mix_all::MASTER_ID {
            self.message_store.put_message(message_ext).await
        } else if self.broker_config.enable_slave_acting_master
            && self.broker_config.enable_remote_escape
        {
            message_ext.set_wait_store_msg_ok(false);
            let send_result = self.put_message_to_remote_broker(message_ext, None).await;
            transform_send_result2put_result(send_result)
        } else {
            PutMessageResult::new_default(PutMessageStatus::ServiceNotAvailable)
        }
    }

    pub async fn put_message_to_remote_broker(
        &mut self,
        _message_ext: MessageExtBrokerInner,
        _broker_name_to_send: Option<CheetahString>,
    ) -> Option<SendResult> {
        unimplemented!("EscapeBridge putMessageToRemoteBroker")
    }
}

#[inline]
fn transform_send_result2put_result(send_result: Option<SendResult>) -> PutMessageResult {
    match send_result {
        None => PutMessageResult::new(PutMessageStatus::PutToRemoteBrokerFail, None, true),
        Some(result) => match result.send_status {
            SendStatus::SendOk => PutMessageResult::new(PutMessageStatus::PutOk, None, true),
            SendStatus::FlushDiskTimeout => {
                PutMessageResult::new(PutMessageStatus::FlushDiskTimeout, None, true)
            }
            SendStatus::FlushSlaveTimeout => {
                PutMessageResult::new(PutMessageStatus::FlushSlaveTimeout, None, true)
            }
            SendStatus::SlaveNotAvailable => {
                PutMessageResult::new(PutMessageStatus::SlaveNotAvailable, None, true)
            }
        },
    }
}

#[cfg(test)]
mod tests {
    use rocketmq_client_rust::producer::send_result::SendResult;
    use rocketmq_client_rust::producer::send_status::SendStatus;

    use super::*;

    #[test]
    fn transform_send_result2put_result_handles_none() {
        let result = transform_send_result2put_result(None);
        assert_eq!(
            result.put_message_status(),
            PutMessageStatus::PutToRemoteBrokerFail
        );
    }

    #[test]
    fn transform_send_result2put_result_handles_send_ok() {
        let send_result = SendResult {
            send_status: SendStatus::SendOk,
            ..Default::default()
        };
        let result = transform_send_result2put_result(Some(send_result));
        assert_eq!(result.put_message_status(), PutMessageStatus::PutOk);
    }

    #[test]
    fn transform_send_result2put_result_handles_flush_disk_timeout() {
        let send_result = SendResult {
            send_status: SendStatus::FlushDiskTimeout,
            ..Default::default()
        };
        let result = transform_send_result2put_result(Some(send_result));
        assert_eq!(
            result.put_message_status(),
            PutMessageStatus::FlushDiskTimeout
        );
    }

    #[test]
    fn transform_send_result2put_result_handles_flush_slave_timeout() {
        let send_result = SendResult {
            send_status: SendStatus::FlushSlaveTimeout,
            ..Default::default()
        };
        let result = transform_send_result2put_result(Some(send_result));
        assert_eq!(
            result.put_message_status(),
            PutMessageStatus::FlushSlaveTimeout
        );
    }

    #[test]
    fn transform_send_result2put_result_handles_slave_not_available() {
        let send_result = SendResult {
            send_status: SendStatus::SlaveNotAvailable,
            ..Default::default()
        };
        let result = transform_send_result2put_result(Some(send_result));
        assert_eq!(
            result.put_message_status(),
            PutMessageStatus::SlaveNotAvailable
        );
    }
}
