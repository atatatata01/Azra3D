use glam::{Vec2, Vec3};
use core_math::half_edge::{HalfEdgeMesh, FaceId, NO_EDGE};
fn check(name:&str, pts:&[Vec3], through:bool){
    let mut m=HalfEdgeMesh::new_box(Vec3::ONE);
    let project=|p:Vec3|->Option<Vec2>{Some(Vec2::new(p.x,p.y-0.5*p.z))};
    let vd=Vec3::new(0.0,-1.0,-1.0).normalize();
    let poly:Vec<Vec2>=pts.iter().map(|&p|project(p).unwrap()).collect();
    core_math::knife::knife_cut_screen(&mut m,&project,&poly,Some(pts),false,through,vd, None, None);
    m.recompute_normals();
    // center of the box is origin; every face normal must point AWAY from origin (outward).
    let mut flipped=0; let mut total=0;
    for f in 0..m.faces.len() as u32 { let fid=FaceId(f); if m.face(fid).half_edge==NO_EDGE{continue;}
        let vs=m.face_vertices(fid); if vs.len()<3 {continue;}
        let c:Vec3=vs.iter().map(|&v|m.vert(v).position).sum::<Vec3>()/vs.len() as f32;
        let n=m.compute_face_normal(fid);
        total+=1;
        if n.dot(c) < 0.0 { flipped+=1; eprintln!("  {name}: face {f} normal INWARD (n={n:?} c={c:?})"); }
    }
    assert_eq!(flipped, 0, "{name}: {flipped}/{total} faces have inward (flipped) normals -> renders wrong/see-through");
}
#[test]
fn normals_outward_after_cut(){
    check("through", &[Vec3::new(-1.0,1.0,0.2),Vec3::new(1.0,1.0,-0.2)], false);
    check("wrap2", &[Vec3::new(-1.0,1.0,0.3),Vec3::new(0.3,0.3,1.0),Vec3::new(-1.0,-0.3,1.0)], false);
    check("user_slit", &[Vec3::new(0.0,1.0,-0.2),Vec3::new(0.2,1.0,1.0),Vec3::new(0.0,0.4,1.0),Vec3::new(0.4,0.1,1.0),Vec3::new(-0.2,-0.4,1.0)], false);
}
