// Copyright 2018 MaidSafe.net limited.
//
// This SAFE Network Software is licensed to you under The General Public License (GPL), version 3.
// Unless required by applicable law or agreed to in writing, the SAFE Network Software distributed
// under the GPL Licence is distributed on an "AS IS" BASIS, WITHOUT WARRANTIES OR CONDITIONS OF ANY
// KIND, either express or implied. Please review the Licences for the specific language governing
// permissions and limitations relating to use of the SAFE Network Software.

use quick_error::quick_error;

quick_error! {
    /// Routing table error variants.
    #[derive(Debug, PartialEq, Eq)]
    pub enum Error {
        /// Adding our own name to the routing table is disallowed.
        OwnNameDisallowed {
            description("Own name disallowed")
            display("Our own name is not allowed to be added to the routing table.")
        }
        /// The peer name to be added doesn't fall within any section in the routing table.
        PeerNameUnsuitable {
            description("Peer name unsuitable")
            display("Peer's name can't be added to the routing table as it's outwith all sections.")
        }
        /// The peer name to be added already exists in the routing table.
        AlreadyExists {
            description("Peer name already exists")
            display("Peer's name has already been added to the routing table.")
        }
        /// The destination section doesn't have enough members to satisfy the requested route.
        CannotRoute {
            description("Can't use requested route")
            display("Destination section doesn't have enough members to use requested route.")
        }
        /// The target peer doesn't exist on the network.  (If it did, it would be in our own
        /// section and we would know of it).
        NoSuchPeer {
            description("No such peer")
            display("Peer doesn't exist on the network.")
        }
        /// The routing table state violates the network invariant
        InvariantViolation {
            description("Network invariant violation")
            display("The routing table state violates the network invariant.")
        }
    }
}
