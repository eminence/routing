// Copyright 2019 MaidSafe.net limited.
//
// This SAFE Network Software is licensed to you under The General Public License (GPL), version 3.
// Unless required by applicable law or agreed to in writing, the SAFE Network Software distributed
// under the GPL Licence is distributed on an "AS IS" BASIS, WITHOUT WARRANTIES OR CONDITIONS OF ANY
// KIND, either express or implied. Please review the Licences for the specific language governing
// permissions and limitations relating to use of the SAFE Network Software.

use super::{
    adult::{Adult, AdultDetails},
    bootstrapping_peer::BootstrappingPeer,
    common::Base,
};
use crate::{
    chain::GenesisPfxInfo,
    error::{InterfaceError, RoutingError},
    id::{FullId, PublicId},
    messages::{
        DirectMessage, HopMessage, MessageContent, RelocatePayload, RoutingMessage,
        SignedRoutingMessage,
    },
    outbox::EventBox,
    peer_map::PeerMap,
    routing_message_filter::RoutingMessageFilter,
    routing_table::Authority,
    state_machine::{State, Transition},
    timer::Timer,
    xor_name::XorName,
    ConnectionInfo, NetworkService,
};
use std::{
    fmt::{self, Display, Formatter},
    time::Duration,
};

/// Time after which bootstrap is cancelled (and possibly retried).
pub const JOIN_TIMEOUT: Duration = Duration::from_secs(120);
/// How many times will the node try to join the same section before giving up and rebootstrapping.
const MAX_JOIN_ATTEMPTS: u8 = 3;

// State of a node after bootstrapping, while joining a section
pub struct JoiningPeer {
    network_service: NetworkService,
    routing_msg_filter: RoutingMessageFilter,
    msg_backlog: Vec<SignedRoutingMessage>,
    full_id: FullId,
    min_section_size: usize,
    peer_map: PeerMap,
    timer: Timer,
    join_token: u64,
    join_attempts: u8,
    conn_infos: Vec<ConnectionInfo>,
    relocate_payload: Option<RelocatePayload>,
}

impl JoiningPeer {
    pub fn new(
        network_service: NetworkService,
        full_id: FullId,
        min_section_size: usize,
        timer: Timer,
        peer_map: PeerMap,
        conn_infos: Vec<ConnectionInfo>,
        relocate_payload: Option<RelocatePayload>,
    ) -> Self {
        let join_token = timer.schedule(JOIN_TIMEOUT);

        let mut joining_peer = Self {
            network_service,
            routing_msg_filter: RoutingMessageFilter::new(),
            msg_backlog: vec![],
            full_id,
            min_section_size,
            timer: timer,
            peer_map,
            join_token,
            join_attempts: 0,
            conn_infos,
            relocate_payload,
        };

        joining_peer.send_join_requests();
        joining_peer
    }

    pub fn into_adult(
        self,
        gen_pfx_info: GenesisPfxInfo,
        outbox: &mut dyn EventBox,
    ) -> Result<State, RoutingError> {
        let details = AdultDetails {
            network_service: self.network_service,
            event_backlog: vec![],
            full_id: self.full_id,
            gen_pfx_info,
            min_section_size: self.min_section_size,
            msg_backlog: self.msg_backlog,
            peer_map: self.peer_map,
            routing_msg_filter: self.routing_msg_filter,
            timer: self.timer,
        };
        Adult::from_joining_peer(details, outbox).map(State::Adult)
    }

    pub fn rebootstrap(self) -> Result<State, RoutingError> {
        Ok(State::BootstrappingPeer(BootstrappingPeer::new(
            self.network_service,
            FullId::new(),
            self.min_section_size,
            self.timer,
        )))
    }

    fn send_join_requests(&mut self) {
        let conn_infos = self.conn_infos.clone();
        for dst in conn_infos {
            info!("{} - Sending JoinRequest to {:?}", self, dst);
            self.send_direct_message(
                &dst,
                DirectMessage::JoinRequest(self.relocate_payload.clone()),
            );
        }
    }

    fn dispatch_routing_message(
        &mut self,
        msg: SignedRoutingMessage,
        _outbox: &mut dyn EventBox,
    ) -> Result<Transition, RoutingError> {
        let (msg, metadata) = msg.into_parts();

        match msg {
            RoutingMessage {
                content: MessageContent::NodeApproval(gen_info),
                src: Authority::PrefixSection(_),
                dst: Authority::Node { .. },
            } => Ok(self.handle_node_approval(gen_info)),
            _ => {
                debug!(
                    "{} - Unhandled routing message, adding to backlog: {:?}",
                    self, msg
                );
                self.msg_backlog
                    .push(SignedRoutingMessage::from_parts(msg, metadata));
                Ok(Transition::Stay)
            }
        }
    }

    fn handle_node_approval(&mut self, gen_pfx_info: GenesisPfxInfo) -> Transition {
        info!(
            "{} - This node has been approved to join the network!",
            self
        );
        Transition::IntoAdult { gen_pfx_info }
    }

    #[cfg(feature = "mock_base")]
    pub fn get_timed_out_tokens(&mut self) -> Vec<u64> {
        self.timer.get_timed_out_tokens()
    }
}

impl Base for JoiningPeer {
    fn network_service(&self) -> &NetworkService {
        &self.network_service
    }

    fn network_service_mut(&mut self) -> &mut NetworkService {
        &mut self.network_service
    }

    fn full_id(&self) -> &FullId {
        &self.full_id
    }

    fn in_authority(&self, dst: &Authority<XorName>) -> bool {
        dst.is_single() && dst.name() == *self.full_id.public_id().name()
    }

    fn min_section_size(&self) -> usize {
        self.min_section_size
    }

    fn peer_map(&self) -> &PeerMap {
        &self.peer_map
    }

    fn peer_map_mut(&mut self) -> &mut PeerMap {
        &mut self.peer_map
    }

    fn timer(&mut self) -> &mut Timer {
        &mut self.timer
    }

    fn handle_send_message(
        &mut self,
        _: Authority<XorName>,
        _: Authority<XorName>,
        _: Vec<u8>,
    ) -> Result<(), InterfaceError> {
        warn!("{} - Cannot handle SendMessage - not joined.", self);
        // TODO: return Err here eventually. Returning Ok for now to
        // preserve the pre-refactor behaviour.
        Ok(())
    }

    fn handle_timeout(&mut self, token: u64, _: &mut dyn EventBox) -> Transition {
        if self.join_token == token {
            self.join_attempts += 1;
            debug!(
                "{} - Timeout when trying to join a section (attempt {}/{}).",
                self, self.join_attempts, MAX_JOIN_ATTEMPTS
            );

            if self.join_attempts < MAX_JOIN_ATTEMPTS {
                self.join_token = self.timer.schedule(JOIN_TIMEOUT);
                self.send_join_requests();
            } else {
                for peer_addr in self
                    .peer_map
                    .remove_all()
                    .map(|conn_info| conn_info.peer_addr)
                {
                    self.network_service
                        .service_mut()
                        .disconnect_from(peer_addr);
                }

                return Transition::Rebootstrap;
            }
        }

        Transition::Stay
    }

    fn handle_direct_message(
        &mut self,
        msg: DirectMessage,
        _pub_id: PublicId,
        _outbox: &mut dyn EventBox,
    ) -> Result<Transition, RoutingError> {
        debug!("{} Unhandled direct message: {:?}", self, msg);

        Ok(Transition::Stay)
    }

    fn handle_hop_message(
        &mut self,
        msg: HopMessage,
        outbox: &mut dyn EventBox,
    ) -> Result<Transition, RoutingError> {
        let HopMessage { content: msg, .. } = msg;

        if self
            .routing_msg_filter
            .filter_incoming(msg.routing_message())
            .is_new()
            && self.in_authority(&msg.routing_message().dst)
        {
            self.check_signed_message_integrity(&msg)?;
            self.dispatch_routing_message(msg, outbox)
        } else {
            Ok(Transition::Stay)
        }
    }
    fn send_routing_message(&mut self, routing_msg: RoutingMessage) -> Result<(), RoutingError> {
        warn!(
            "{} - Tried to send a routing message: {:?}",
            self, routing_msg
        );
        Ok(())
    }
}

impl Display for JoiningPeer {
    fn fmt(&self, formatter: &mut Formatter) -> fmt::Result {
        write!(formatter, "JoiningPeer({})", self.name())
    }
}
