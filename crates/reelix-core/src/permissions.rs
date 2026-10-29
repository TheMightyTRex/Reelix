use bitflags::bitflags;

bitflags! {
    /// User permissions. `ADMIN` implies every other permission.
    ///
    /// Values are persisted in the database, so existing bits must never be renumbered.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub struct Permissions: u64 {
        const ADMIN              = 1 << 0;
        const MANAGE_LIBRARY     = 1 << 1;
        const MANAGE_REQUESTS    = 1 << 2;
        const MANAGE_USERS       = 1 << 3;
        const MANAGE_ISSUES      = 1 << 4;
        const REQUEST_MOVIE      = 1 << 5;
        const REQUEST_TV         = 1 << 6;
        const REQUEST_4K         = 1 << 7;
        const REQUEST_ADVANCED   = 1 << 8;
        const AUTO_APPROVE_MOVIE = 1 << 9;
        const AUTO_APPROVE_TV    = 1 << 10;
        const AUTO_APPROVE_4K    = 1 << 11;
        const VIEW_REQUESTS      = 1 << 12;
        const CREATE_ISSUES      = 1 << 13;
        const VIEW_CALENDAR      = 1 << 14;
        const VIEW_QUEUE         = 1 << 15;
        const REQUEST_BOOK       = 1 << 16;
        const REQUEST_MUSIC      = 1 << 17;
        const AUTO_APPROVE_BOOK  = 1 << 18;
        const AUTO_APPROVE_MUSIC = 1 << 19;
    }
}

impl Permissions {
    /// Role preset: full control.
    pub const ROLE_ADMIN: Self = Self::ADMIN;

    /// Role preset: manages library, requests, users and issues, but not system settings.
    pub const ROLE_MANAGER: Self = Self::MANAGE_LIBRARY
        .union(Self::MANAGE_REQUESTS)
        .union(Self::MANAGE_USERS)
        .union(Self::MANAGE_ISSUES)
        .union(Self::ROLE_POWER_USER);

    /// Role preset: advanced requester with auto-approval.
    pub const ROLE_POWER_USER: Self = Self::ROLE_USER
        .union(Self::REQUEST_4K)
        .union(Self::REQUEST_ADVANCED)
        .union(Self::AUTO_APPROVE_MOVIE)
        .union(Self::AUTO_APPROVE_TV)
        .union(Self::AUTO_APPROVE_4K)
        .union(Self::VIEW_REQUESTS)
        .union(Self::REQUEST_BOOK)
        .union(Self::REQUEST_MUSIC)
        .union(Self::AUTO_APPROVE_BOOK)
        .union(Self::AUTO_APPROVE_MUSIC);

    /// Role preset: basic requester.
    pub const ROLE_USER: Self = Self::REQUEST_MOVIE
        .union(Self::REQUEST_TV)
        .union(Self::CREATE_ISSUES);

    /// Whether these permissions grant `required`. Admins pass every check.
    pub fn has(self, required: Self) -> bool {
        self.contains(Self::ADMIN) || self.contains(required)
    }

    /// Whether these permissions grant at least one bit of `any`. Admins pass every check.
    pub fn has_any(self, any: Self) -> bool {
        self.contains(Self::ADMIN) || self.intersects(any)
    }

    pub fn is_admin(self) -> bool {
        self.contains(Self::ADMIN)
    }
}

// Serialized as the raw bit value; unknown bits from newer versions are dropped.
impl serde::Serialize for Permissions {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_u64(self.bits())
    }
}

impl<'de> serde::Deserialize<'de> for Permissions {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Ok(Self::from_bits_truncate(u64::deserialize(d)?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn admin_implies_everything() {
        assert!(Permissions::ADMIN.has(Permissions::MANAGE_LIBRARY | Permissions::REQUEST_4K));
    }

    #[test]
    fn roles_are_nested() {
        assert!(Permissions::ROLE_MANAGER.contains(Permissions::ROLE_POWER_USER));
        assert!(Permissions::ROLE_POWER_USER.contains(Permissions::ROLE_USER));
        assert!(!Permissions::ROLE_MANAGER.is_admin());
    }

    #[test]
    fn missing_permission_is_denied() {
        assert!(!Permissions::ROLE_USER.has(Permissions::MANAGE_LIBRARY));
    }

    #[test]
    fn has_any_needs_one_bit() {
        let either = Permissions::MANAGE_LIBRARY | Permissions::VIEW_QUEUE;
        assert!((Permissions::ROLE_POWER_USER | Permissions::VIEW_QUEUE).has_any(either));
        assert!(!Permissions::ROLE_POWER_USER.has_any(either));
        assert!(Permissions::ADMIN.has_any(either));
    }

    #[test]
    fn book_and_music_bits_are_stable() {
        assert_eq!(Permissions::REQUEST_BOOK.bits(), 1 << 16);
        assert_eq!(Permissions::REQUEST_MUSIC.bits(), 1 << 17);
        assert_eq!(Permissions::AUTO_APPROVE_BOOK.bits(), 1 << 18);
        assert_eq!(Permissions::AUTO_APPROVE_MUSIC.bits(), 1 << 19);
        let content = Permissions::REQUEST_BOOK
            | Permissions::REQUEST_MUSIC
            | Permissions::AUTO_APPROVE_BOOK
            | Permissions::AUTO_APPROVE_MUSIC;
        assert!(!Permissions::ROLE_USER.intersects(content));
        assert!(Permissions::ROLE_POWER_USER.contains(content));
    }

    #[test]
    fn serializes_as_bits() {
        let json = serde_json::to_string(&(Permissions::ADMIN | Permissions::REQUEST_TV)).unwrap();
        assert_eq!(json, "65");
    }
}
