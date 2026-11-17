use super::*;

#[test]
fn a_revision_evicted_for_capacity_needs_its_data_again() {
    let mut publications = CachePublications::default();
    publications.set_interest(Vec::new());
    let page = |sequence| PageId {
        context_epoch: 1,
        sequence,
    };
    let (first, _) = publications.publish(
        page(0),
        PageCacheKey::Rows,
        "rows",
        Vec::new(),
        Publication::Projection,
    );
    publications.set_interest(vec![first.clone()]);

    for sequence in 1..=MAX_CACHE_ENTRIES as u64 {
        publications.publish(
            page(sequence),
            PageCacheKey::Rows,
            "rows",
            Vec::new(),
            Publication::Projection,
        );
    }
    assert_eq!(
        publications.take_invalidated(),
        vec![(ResourceCacheInvalidation::Evicted, first.clone())]
    );

    publications.set_interest(vec![first.clone()]);
    let (new, data) = publications.publish(
        page(0),
        PageCacheKey::Rows,
        "rows",
        Vec::new(),
        Publication::Projection,
    );
    assert_ne!(new.revision, first.revision);
    assert_eq!(data, Some("rows"));
}
