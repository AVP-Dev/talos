#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExecutionMode {
    Auto,
    Semi,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Action {
    WriterLaunch,
    ProjectCheck,
    CriticReview,
    CriticPatchApplication,
    GitMutation,
    Publish,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Authorization {
    Allow,
    RequireOperatorApproval,
    Deny,
}

pub fn authorize(mode: ExecutionMode, action: Action, allowlisted: bool) -> Authorization {
    if matches!(
        action,
        Action::CriticPatchApplication | Action::GitMutation | Action::Publish
    ) {
        return Authorization::Deny;
    }
    if !allowlisted {
        return Authorization::Deny;
    }
    match (mode, action) {
        (ExecutionMode::Auto, Action::WriterLaunch | Action::ProjectCheck) => Authorization::Allow,
        (ExecutionMode::Semi, Action::WriterLaunch | Action::ProjectCheck) => {
            Authorization::RequireOperatorApproval
        }
        _ => Authorization::Deny,
    }
}

#[cfg(test)]
mod tests {
    use super::{Action, Authorization, ExecutionMode, authorize};

    #[test]
    fn auto_only_allows_allowlisted_local_work() {
        assert_eq!(
            authorize(ExecutionMode::Auto, Action::WriterLaunch, true),
            Authorization::Allow
        );
        assert_eq!(
            authorize(ExecutionMode::Auto, Action::ProjectCheck, false),
            Authorization::Deny
        );
        assert_eq!(
            authorize(ExecutionMode::Auto, Action::Publish, true),
            Authorization::Deny
        );
    }

    #[test]
    fn semi_requires_approval_and_hard_denies_override_the_allowlist() {
        assert_eq!(
            authorize(ExecutionMode::Semi, Action::WriterLaunch, true),
            Authorization::RequireOperatorApproval
        );
        assert_eq!(
            authorize(ExecutionMode::Semi, Action::ProjectCheck, true),
            Authorization::RequireOperatorApproval
        );
        for action in [
            Action::CriticPatchApplication,
            Action::GitMutation,
            Action::Publish,
        ] {
            assert_eq!(
                authorize(ExecutionMode::Semi, action, true),
                Authorization::Deny
            );
        }
    }
}
