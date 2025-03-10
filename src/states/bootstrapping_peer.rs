// Copyright 2018 MaidSafe.net limited.
//
// This SAFE Network Software is licensed to you under The General Public License (GPL), version 3.
// Unless required by applicable law or agreed to in writing, the SAFE Network Software distributed
// under the GPL Licence is distributed on an "AS IS" BASIS, WITHOUT WARRANTIES OR CONDITIONS OF ANY
// KIND, either express or implied. Please review the Licences for the specific language governing
// permissions and limitations relating to use of the SAFE Network Software.

use super::common::Base;
use crate::{
    error::{InterfaceError, RoutingError},
    event::Event,
    id::{FullId, PublicId},
    messages::{
        BootstrapResponse, DirectMessage, HopMessage, RelocatePayload, RoutingMessage,
        SignedRelocateDetails,
    },
    outbox::EventBox,
    peer_map::PeerMap,
    routing_table::{Authority, Prefix},
    state_machine::{State, Transition},
    states::JoiningPeer,
    timer::Timer,
    xor_name::XorName,
    ConnectionInfo, NetworkService,
};
use log::LogLevel;
use std::{
    collections::HashSet,
    fmt::{self, Display, Formatter},
    net::SocketAddr,
    time::Duration,
};

/// Time after which bootstrap is cancelled (and possibly retried).
pub const BOOTSTRAP_TIMEOUT: Duration = Duration::from_secs(20);

// State of Client or Node while bootstrapping.
pub struct BootstrappingPeer {
    nodes_to_await: HashSet<SocketAddr>,
    bootstrap_connection: Option<(ConnectionInfo, u64)>,
    network_service: NetworkService,
    full_id: FullId,
    min_section_size: usize,
    peer_map: PeerMap,
    timer: Timer,
    relocate_details: Option<SignedRelocateDetails>,
}

impl BootstrappingPeer {
    /// Create `BootstrappingPeer` for a node that is joining the network (either for the first
    /// time or rejoining).
    pub fn new(
        mut network_service: NetworkService,
        full_id: FullId,
        min_section_size: usize,
        timer: Timer,
    ) -> Self {
        network_service.service_mut().bootstrap();
        Self {
            network_service,
            full_id,
            min_section_size,
            timer,
            bootstrap_connection: None,
            nodes_to_await: Default::default(),
            peer_map: PeerMap::new(),
            relocate_details: None,
        }
    }

    /// Create `BootstrappingPeer` for a node that is being relocated into another sections.
    pub fn relocate(
        network_service: NetworkService,
        full_id: FullId,
        min_section_size: usize,
        timer: Timer,
        conn_infos: Vec<ConnectionInfo>,
        relocate_details: SignedRelocateDetails,
    ) -> Self {
        let mut node = Self {
            network_service,
            full_id,
            min_section_size,
            timer,
            bootstrap_connection: None,
            nodes_to_await: conn_infos.iter().map(|info| info.peer_addr).collect(),
            peer_map: PeerMap::new(),
            relocate_details: Some(relocate_details),
        };

        for conn_info in conn_infos {
            node.send_bootstrap_request(conn_info)
        }

        node
    }

    pub fn into_joining(
        self,
        conn_infos: Vec<ConnectionInfo>,
        relocate_payload: Option<RelocatePayload>,
        _outbox: &mut dyn EventBox,
    ) -> Result<State, RoutingError> {
        Ok(State::JoiningPeer(JoiningPeer::new(
            self.network_service,
            self.full_id,
            self.min_section_size,
            self.timer,
            self.peer_map,
            conn_infos,
            relocate_payload,
        )))
    }

    fn send_bootstrap_request(&mut self, dst: ConnectionInfo) {
        let _ = self.nodes_to_await.remove(&dst.peer_addr);

        if let Some((bootstrap_dst, _)) = self.bootstrap_connection.as_ref() {
            if *bootstrap_dst != dst {
                // we already have an active connection, drop this one
                self.network_service
                    .service_mut()
                    .disconnect_from(dst.peer_addr);
            }
        } else {
            debug!("{} Sending BootstrapRequest to {}.", self, dst.peer_addr);

            let token = self.timer.schedule(BOOTSTRAP_TIMEOUT);
            self.bootstrap_connection = Some((dst.clone(), token));

            // If we are relocating, request bootstrap to the section matching the name given to us
            // by our section. Otherwise request bootstrap to the section matching our current name.
            let destination = if let Some(details) = self.relocate_details.as_ref() {
                details.content().destination
            } else {
                *self.name()
            };

            self.send_direct_message(&dst, DirectMessage::BootstrapRequest(destination));
            self.peer_map_mut().connect(dst);
        }
    }

    fn join_section(
        &mut self,
        prefix: Prefix<XorName>,
        conn_infos: Vec<ConnectionInfo>,
    ) -> Result<Transition, RoutingError> {
        let old_full_id = self.full_id.clone();

        if !prefix.matches(self.name()) {
            let new_full_id = FullId::within_range(&prefix.range_inclusive());
            info!(
                "{} - Changing name to {}.",
                self,
                new_full_id.public_id().name()
            );
            self.full_id = new_full_id;
        }

        let relocate_payload = if let Some(details) = self.relocate_details.take() {
            Some(RelocatePayload::new(
                details,
                self.full_id.public_id(),
                &old_full_id,
            )?)
        } else {
            None
        };

        Ok(Transition::IntoJoining {
            conn_infos,
            relocate_payload,
        })
    }

    fn reconnect_to_new_section(&mut self, new_conn_infos: Vec<ConnectionInfo>) {
        if let Some((conn_info, _)) = self.bootstrap_connection.take() {
            debug!(
                "{} Dropping connected node at {} and retrying.",
                self, conn_info.peer_addr
            );

            // drop the current connection
            self.network_service
                .service_mut()
                .disconnect_from(conn_info.peer_addr);
        }

        self.nodes_to_await = new_conn_infos
            .iter()
            .map(|conn_info| conn_info.peer_addr)
            .collect();

        for conn_info in new_conn_infos {
            self.network_service.service_mut().connect_to(conn_info);
        }
    }

    fn disconnect_from_bootstrap_proxy(&mut self) {
        if let Some((conn_info, _)) = self.bootstrap_connection.take() {
            debug!(
                "{} Dropping bootstrap node at {} and retrying.",
                self, conn_info.peer_addr
            );

            self.network_service
                .service_mut()
                .disconnect_from(conn_info.peer_addr);
        }
    }

    fn rebootstrap(&mut self) {
        // only rebootstrap if we're not waiting for connections from anyone else -
        // otherwise we'll just wait and maybe another connection succeeds
        if !self.nodes_to_await.is_empty() {
            return;
        }

        self.disconnect_from_bootstrap_proxy();
        self.network_service.service_mut().bootstrap();
    }
}

impl Base for BootstrappingPeer {
    fn network_service(&self) -> &NetworkService {
        &self.network_service
    }

    fn network_service_mut(&mut self) -> &mut NetworkService {
        &mut self.network_service
    }

    fn full_id(&self) -> &FullId {
        &self.full_id
    }

    fn in_authority(&self, _: &Authority<XorName>) -> bool {
        false
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
        warn!("{} - Cannot handle SendMessage - not bootstrapped.", self);
        // TODO: return Err here eventually. Returning Ok for now to
        // preserve the pre-refactor behaviour.
        Ok(())
    }

    fn handle_timeout(&mut self, token: u64, _: &mut dyn EventBox) -> Transition {
        if let Some((conn_info, bootstrap_token)) = self.bootstrap_connection.as_ref() {
            if *bootstrap_token == token {
                debug!(
                    "{} - Timeout when trying to bootstrap against {}.",
                    self, conn_info.peer_addr
                );

                self.disconnect_from_bootstrap_proxy();
                self.rebootstrap();
            }
        }

        Transition::Stay
    }

    fn handle_bootstrapped_to(&mut self, conn_info: ConnectionInfo) -> Transition {
        self.peer_map_mut().connect(conn_info.clone());

        if self.bootstrap_connection.is_none() {
            debug!(
                "{} Received BootstrappedTo event from {}.",
                self, conn_info.peer_addr
            );

            // Established connection. Pending Validity checks
            self.send_bootstrap_request(conn_info);
        } else {
            warn!("{} Received more than one BootstrappedTo event", self);
        }

        Transition::Stay
    }

    fn handle_bootstrap_failure(&mut self, outbox: &mut dyn EventBox) -> Transition {
        info!("{} Failed to bootstrap. Terminating.", self);
        outbox.send_event(Event::Terminated);
        Transition::Terminate
    }

    fn handle_connected_to(
        &mut self,
        conn_info: ConnectionInfo,
        _outbox: &mut dyn EventBox,
    ) -> Transition {
        debug!(
            "{} Received ConnectedTo event from {}.",
            self, conn_info.peer_addr
        );

        self.send_bootstrap_request(conn_info);
        Transition::Stay
    }

    fn handle_connection_failure(
        &mut self,
        peer_addr: SocketAddr,
        _: &mut dyn EventBox,
    ) -> Transition {
        let _ = self.nodes_to_await.remove(&peer_addr);
        let _ = self.peer_map_mut().disconnect(peer_addr);

        if let Some((conn_info, _)) = self.bootstrap_connection.as_ref() {
            if conn_info.peer_addr == peer_addr {
                info!("{} Lost connection to proxy {}.", self, peer_addr);
                self.disconnect_from_bootstrap_proxy();
                self.rebootstrap();
            }
        }

        Transition::Stay
    }

    fn handle_direct_message(
        &mut self,
        msg: DirectMessage,
        _pub_id: PublicId,
        _: &mut dyn EventBox,
    ) -> Result<Transition, RoutingError> {
        match msg {
            DirectMessage::BootstrapResponse(BootstrapResponse::Join { prefix, conn_infos }) => {
                info!(
                    "{} - Joining a section {:?}: {:?}",
                    self, prefix, conn_infos
                );
                self.join_section(prefix, conn_infos)
            }
            DirectMessage::BootstrapResponse(BootstrapResponse::Rebootstrap(new_conn_infos)) => {
                info!(
                    "{} - Bootstrapping redirected to another set of peers: {:?}",
                    self, new_conn_infos
                );
                self.reconnect_to_new_section(new_conn_infos);
                Ok(Transition::Stay)
            }
            _ => {
                debug!("{} - Unhandled direct message: {:?}", self, msg);
                Ok(Transition::Stay)
            }
        }
    }

    fn handle_hop_message(
        &mut self,
        msg: HopMessage,
        _: &mut dyn EventBox,
    ) -> Result<Transition, RoutingError> {
        debug!("{} - Unhandled hop message: {:?}", self, msg);
        Ok(Transition::Stay)
    }

    fn send_routing_message(&mut self, routing_msg: RoutingMessage) -> Result<(), RoutingError> {
        log_or_panic!(
            LogLevel::Error,
            "{} - Tried to send a routing message: {:?}",
            self,
            routing_msg
        );
        Ok(())
    }
}

impl Display for BootstrappingPeer {
    fn fmt(&self, formatter: &mut Formatter) -> fmt::Result {
        write!(formatter, "BootstrappingPeer({})", self.name())
    }
}

#[cfg(all(test, feature = "mock_base"))]
mod tests {
    use super::*;
    use crate::{
        id::FullId,
        messages::Message,
        mock::Network,
        outbox::EventBuf,
        quic_p2p::{Builder, Peer},
        state_machine::StateMachine,
        states::common::from_network_bytes,
        NetworkConfig, NetworkEvent,
    };
    use crossbeam_channel as mpmc;
    use unwrap::unwrap;

    #[test]
    // Check that losing our proxy connection while in the `BootstrappingPeer` state doesn't stall
    // and instead triggers a re-bootstrap attempt..
    fn lose_proxy_connection() {
        let min_section_size = 8;
        let network = Network::new(min_section_size, None);

        // Start a bare-bones network service.
        let (event_tx, event_rx) = mpmc::unbounded();
        let node_a_endpoint = network.gen_addr();
        let config = NetworkConfig::node().with_endpoint(node_a_endpoint);
        let node_a_network_service = unwrap!(Builder::new(event_tx).with_config(config).build());

        // Construct a `StateMachine` which will start in the `BootstrappingPeer` state and
        // bootstrap off the network service above.
        let node_b_endpoint = network.gen_addr();
        let config = NetworkConfig::client()
            .with_hard_coded_contact(node_a_endpoint)
            .with_endpoint(node_b_endpoint);
        let node_b_full_id = FullId::new();
        let mut node_b_outbox = EventBuf::new();
        let (_node_b_action_tx, mut node_b_state_machine) = StateMachine::new(
            move |network_service, timer, _outbox2| {
                State::BootstrappingPeer(BootstrappingPeer::new(
                    network_service,
                    node_b_full_id,
                    min_section_size,
                    timer,
                ))
            },
            config,
            &mut node_b_outbox,
        );

        // Check the network service received `ConnectedTo`.
        network.poll();
        match unwrap!(event_rx.try_recv()) {
            NetworkEvent::ConnectedTo {
                peer: Peer::Client { .. },
            } => (),
            _ => panic!("Should have received `ConnectedTo` event."),
        }

        // The state machine should have received the `BootstrappedTo` event and this will have
        // caused it to send a `BootstrapRequest` message.
        network.poll();
        step_at_least_once(&mut node_b_state_machine, &mut node_b_outbox);

        // Check the network service received the `BootstrapRequest`
        network.poll();
        if let NetworkEvent::NewMessage { peer_addr, msg } = unwrap!(event_rx.try_recv()) {
            assert_eq!(peer_addr, node_b_endpoint);

            let ok = match unwrap!(from_network_bytes(msg)) {
                Message::Direct(msg) => match *msg.content() {
                    DirectMessage::BootstrapRequest(_) => true,
                    _ => false,
                },
                _ => false,
            };

            if !ok {
                panic!("Should have received a `BootstrapRequest`.");
            }
        } else {
            panic!("Should have received `NewMessage` event.");
        }

        // Drop the network service...
        drop(node_a_network_service);
        network.poll();

        // ...which triggers `ConnectionFailure` on the state machine which then attempts to
        // rebootstrap..
        step_at_least_once(&mut node_b_state_machine, &mut node_b_outbox);
        assert!(node_b_outbox.take_all().is_empty());
        network.poll();

        // ... but there is no one to bootstrap to, so the bootstrap fails which causes the state
        // machine to terminate.
        step_at_least_once(&mut node_b_state_machine, &mut node_b_outbox);
        let events = node_b_outbox.take_all();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0], Event::Terminated);
    }

    fn step_at_least_once(machine: &mut StateMachine, outbox: &mut dyn EventBox) {
        // Blocking step for the first one. Must not err.
        unwrap!(machine.step(outbox));
        // Exhaust any remaining step
        while machine.try_step(outbox).is_ok() {}
    }
}
