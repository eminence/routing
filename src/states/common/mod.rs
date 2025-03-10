// Copyright 2018 MaidSafe.net limited.
//
// This SAFE Network Software is licensed to you under The General Public License (GPL), version 3.
// Unless required by applicable law or agreed to in writing, the SAFE Network Software distributed
// under the GPL Licence is distributed on an "AS IS" BASIS, WITHOUT WARRANTIES OR CONDITIONS OF ANY
// KIND, either express or implied. Please review the Licences for the specific language governing
// permissions and limitations relating to use of the SAFE Network Software.

mod approved;
mod base;

pub use self::{
    approved::Approved,
    base::{from_network_bytes, Base},
};
use crate::time::Duration;

pub(super) const GOSSIP_TIMEOUT: Duration = Duration::from_secs(2);
