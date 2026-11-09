//! Constants ported from the original Python Twitch Channel Points Miner v2

pub const CLIENT_ID: &str = "ue6666qo983tsx6so1t0vnawi233wa"; // TV client
pub const CLIENT_VERSION: &str = "ef928475-9403-42f2-8a34-55784bd08e16";
pub const WEBSOCKET_URL: &str = "wss://pubsub-edge.twitch.tv/v1";
pub const GQL_URL: &str = "https://gql.twitch.tv/gql";
pub const INTEGRITY_URL: &str = "https://gql.twitch.tv/integrity";
pub const DROP_ID: &str = "c2542d6d-cd10-4532-919b-3d19f30a768b";
pub const USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36";

/// GQL persisted query operation names and their sha256 hashes
pub mod gql {
    pub struct Op {
        pub name: &'static str,
        pub hash: &'static str,
    }

    pub const WITH_IS_STREAM_LIVE: Op = Op {
        name: "WithIsStreamLiveQuery",
        hash: "04e46329a6786ff3a81c01c50bfa5d725902507a0deb83b0edbf7abe7a3716ea",
    };

    pub const PLAYBACK_ACCESS_TOKEN: Op = Op {
        name: "PlaybackAccessToken",
        hash: "3093517e37e4f4cb48906155bcd894150aef92617939236d2508f3375ab732ce",
    };

    pub const VIDEO_PLAYER_STREAM_INFO: Op = Op {
        name: "VideoPlayerStreamInfoOverlayChannel",
        hash: "e785b65ff71ad7b363b34878335f27dd9372869ad0c5740a130b9268bcdbe7e7",
    };

    pub const CLAIM_COMMUNITY_POINTS: Op = Op {
        name: "ClaimCommunityPoints",
        hash: "46aaeebe02c99afdf4fc97c7c0cba964124bf6b0af229395f1f6d1feed05b3d0",
    };

    pub const COMMUNITY_MOMENT_CLAIM: Op = Op {
        name: "CommunityMomentCallout_Claim",
        hash: "374314de591e69925fce3ddc2bcf085796f56ebb8cad67a0daa3165c03adc345",
    };

    pub const CLAIM_DROP_REWARDS: Op = Op {
        name: "DropsPage_ClaimDropRewards",
        hash: "a455deea71bdc9015b78eb49f4acfbce8baa7ccbedd28e549bb025bd0f751930",
    };

    pub const CHANNEL_POINTS_CONTEXT: Op = Op {
        name: "ChannelPointsContext",
        hash: "1530a003a7d374b0380b79db0be0534f30ff46e61cffa2bc0e2468a909fbc024",
    };

    pub const JOIN_RAID: Op = Op {
        name: "JoinRaid",
        hash: "c6a332a86d1087fbbb1a8623aa01bd1313d2386e7c63be60fdb2d1901f01a4ae",
    };

    pub const INVENTORY: Op = Op {
        name: "Inventory",
        hash: "d86775d0ef16a63a33ad52e80eaff963b2d5b72fada7c991504a57496e1d8e4b",
    };

    pub const MAKE_PREDICTION: Op = Op {
        name: "MakePrediction",
        hash: "b44682ecc88358817009f20e69d75081b1e58825bb40aa53d5dbadcc17c881d8",
    };

    pub const VIEWER_DROPS_DASHBOARD: Op = Op {
        name: "ViewerDropsDashboard",
        hash: "5a4da2ab3d5b47c9f9ce864e727b2cb346af1e3ea8b897fe8f704a97ff017619",
    };

    pub const DROP_CAMPAIGN_DETAILS: Op = Op {
        name: "DropCampaignDetails",
        hash: "f6396f5ffdde867a8f6f6da18286e4baf02e5b98d14689a69b5af320a4c7b7b8",
    };

    pub const AVAILABLE_DROPS: Op = Op {
        name: "DropsHighlightService_AvailableDrops",
        hash: "9a62a09bce5b53e26e64a671e530bc599cb6aab1e5ba3cbd5d85966d3940716f",
    };

    pub const GET_ID_FROM_LOGIN: Op = Op {
        name: "GetIDFromLogin",
        hash: "94e82a7b1e3c21e186daa73ee2afc4b8f23bade1fbbff6fe8ac133f50a2f58ca",
    };

    pub const CHANNEL_FOLLOWS: Op = Op {
        name: "ChannelFollows",
        hash: "eecf815273d3d949e5cf0085cc5084cd8a1b5b7b6f7990cf43cb0beadf546907",
    };

    pub const USER_POINTS_CONTRIBUTION: Op = Op {
        name: "UserPointsContribution",
        hash: "23ff2c2d60708379131178742327ead913b93b1bd6f665517a6d9085b73f661f",
    };

    pub const CONTRIBUTE_COMMUNITY_GOAL: Op = Op {
        name: "ContributeCommunityPointsCommunityGoal",
        hash: "5774f0ea5d89587d73021a2e03c3c44777d903840c608754a1be519f51e37bb6",
    };
}
