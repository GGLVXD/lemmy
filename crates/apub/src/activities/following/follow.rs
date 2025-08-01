use crate::{
  activities::{generate_activity_id, send_lemmy_activity, verify_person},
  protocol::activities::following::{accept::AcceptFollow, follow::Follow},
};
use activitypub_federation::{
  config::Data,
  kinds::activity::FollowType,
  protocol::verification::verify_urls_match,
  traits::{Activity, Actor, Object},
};
use either::Either::*;
use lemmy_api_utils::context::LemmyContext;
use lemmy_apub_objects::{
  objects::{person::ApubPerson, CommunityOrMulti},
  utils::functions::verify_person_in_community,
};
use lemmy_db_schema::{
  source::{
    activity::ActivitySendTargets,
    community::{CommunityActions, CommunityFollowerForm},
    instance::Instance,
    multi_community::{MultiCommunity, MultiCommunityFollowForm},
    person::{PersonActions, PersonFollowerForm},
  },
  traits::Followable,
};
use lemmy_db_schema_file::enums::{CommunityFollowerState, CommunityVisibility};
use lemmy_utils::error::{FederationError, LemmyError, LemmyErrorType, LemmyResult};
use url::Url;

impl Follow {
  pub(in crate::activities::following) fn new(
    actor: &ApubPerson,
    target: &CommunityOrMulti,
    context: &Data<LemmyContext>,
  ) -> LemmyResult<Follow> {
    Ok(Follow {
      actor: actor.id().clone().into(),
      object: target.id().clone().into(),
      to: Some([target.id().clone().into()]),
      kind: FollowType::Follow,
      id: generate_activity_id(FollowType::Follow, context)?,
    })
  }

  pub async fn send(
    actor: &ApubPerson,
    target: &CommunityOrMulti,
    context: &Data<LemmyContext>,
  ) -> LemmyResult<()> {
    let follow = Follow::new(actor, target, context)?;
    let inbox = ActivitySendTargets::to_inbox(target.shared_inbox_or_inbox());
    send_lemmy_activity(context, follow, actor, inbox, true).await
  }
}

#[async_trait::async_trait]
impl Activity for Follow {
  type DataType = LemmyContext;
  type Error = LemmyError;

  fn id(&self) -> &Url {
    &self.id
  }

  fn actor(&self) -> &Url {
    self.actor.inner()
  }

  async fn verify(&self, context: &Data<LemmyContext>) -> LemmyResult<()> {
    verify_person(&self.actor, context).await?;
    let object = self.object.dereference(context).await?;
    if let Right(Left(c)) = object {
      verify_person_in_community(&self.actor, &c, context).await?;
    }
    if let Some(to) = &self.to {
      verify_urls_match(to[0].inner(), self.object.inner())?;
    }
    Ok(())
  }

  async fn receive(self, context: &Data<LemmyContext>) -> LemmyResult<()> {
    use CommunityVisibility::*;
    let actor = self.actor.dereference(context).await?;
    let object = self.object.dereference(context).await?;
    match object {
      Left(u) => {
        let form = PersonFollowerForm::new(u.id, actor.id, false);
        PersonActions::follow(&mut context.pool(), &form).await?;
        AcceptFollow::send(self, context).await?;
      }
      Right(Left(c)) => {
        if c.visibility == CommunityVisibility::Private {
          let instance = Instance::read(&mut context.pool(), actor.instance_id).await?;
          if [Some("kbin"), Some("mbin")].contains(&instance.software.as_deref()) {
            // TODO: change this to a minimum version check once private communities are supported
            return Err(FederationError::PlatformLackingPrivateCommunitySupport.into());
          }
        }
        let follow_state = match c.visibility {
          Public | Unlisted => CommunityFollowerState::Accepted,
          Private => CommunityFollowerState::ApprovalRequired,
          // Dont allow following local-only community via federation.
          LocalOnlyPrivate | LocalOnlyPublic => return Err(LemmyErrorType::NotFound.into()),
        };
        let form = CommunityFollowerForm::new(c.id, actor.id, follow_state);
        CommunityActions::follow(&mut context.pool(), &form).await?;
        if c.visibility == CommunityVisibility::Public {
          AcceptFollow::send(self, context).await?;
        }
      }
      Right(Right(m)) => {
        let form = MultiCommunityFollowForm {
          multi_community_id: m.id,
          person_id: actor.id,
          follow_state: CommunityFollowerState::Accepted,
        };

        MultiCommunity::follow(&mut context.pool(), &form).await?;
        AcceptFollow::send(self, context).await?;
      }
    }
    Ok(())
  }
}
