#![allow(clippy::too_many_arguments)]
use super::errors::ProofVerifyError;
use super::math::Math;
use super::random::RandomTape;
use super::scalar::Scalar;
use super::transcript::{AppendToTranscript, ProofTranscript};
use blake3::traits::digest;
use core::ops::Index;
use digest::Output;
use ff::Field;
use merlin::Transcript;
use ligero_pc::{LigeroCommit, LigeroEncoding, LigeroEvalProof};
use lcpc2d::{LcRoot};
use serde::{Serialize, Deserialize};

type Hasher = blake3::Hasher;

#[cfg(feature = "multicore")]
use rayon::prelude::*;

#[derive(Debug)]
pub struct DensePolynomial {
  num_vars: usize, // the number of variables in the multilinear polynomial
  len: usize,
  Z: Vec<Scalar>, // evaluations of the polynomial in all the 2^num_vars Boolean inputs
}

pub struct PolyCommitmentGens {
  pub gens: usize,
}

impl PolyCommitmentGens {
  // the number of variables in the multilinear polynomial
  pub fn new(num_vars: usize, _label: &'static [u8]) -> PolyCommitmentGens {
    let (_left, right) = EqPolynomial::compute_factored_lens(num_vars);
    let gens = right.pow2();
    PolyCommitmentGens { gens }
  }
}

pub struct PolyCommitmentBlinds {
  blinds: Vec<Scalar>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PolyCommitment {
  C: LcRoot<Hasher, LigeroEncoding<Scalar>>,
}

#[derive(Debug)]
pub struct PolyDecommitment {
  decomm: LigeroCommit<Hasher, Scalar>,
  enc: LigeroEncoding<Scalar>,
}

pub struct EqPolynomial {
  r: Vec<Scalar>,
}

impl EqPolynomial {
  pub fn new(r: Vec<Scalar>) -> Self {
    EqPolynomial { r }
  }

  pub fn evaluate(&self, rx: &[Scalar]) -> Scalar {
    assert_eq!(self.r.len(), rx.len());
    (0..rx.len())
      .map(|i| self.r[i] * rx[i] + (Scalar::one() - self.r[i]) * (Scalar::one() - rx[i]))
      .product()
  }

  pub fn evals(&self) -> Vec<Scalar> {
    let ell = self.r.len();

    let mut evals: Vec<Scalar> = vec![Scalar::one(); ell.pow2()];
    let mut size = 1;
    for j in 0..ell {
      // in each iteration, we double the size of chis
      size *= 2;
      for i in (0..size).rev().step_by(2) {
        // copy each element from the prior iteration twice
        let scalar = evals[i / 2];
        evals[i] = scalar * self.r[j];
        evals[i - 1] = scalar - evals[i];
      }
    }
    evals
  }

  pub fn compute_factored_lens(ell: usize) -> (usize, usize) {
    (ell / 2, ell - ell / 2)
  }

  pub fn compute_factored_evals(&self) -> (Vec<Scalar>, Vec<Scalar>) {
    let ell = self.r.len();
    let (left_num_vars, _right_num_vars) = EqPolynomial::compute_factored_lens(ell);

    let L = EqPolynomial::new(self.r[..left_num_vars].to_vec()).evals();
    let R = EqPolynomial::new(self.r[left_num_vars..ell].to_vec()).evals();

    (L, R)
  }
}

pub struct IdentityPolynomial {
  size_point: usize,
}

impl IdentityPolynomial {
  pub fn new(size_point: usize) -> Self {
    IdentityPolynomial { size_point }
  }

  pub fn evaluate(&self, r: &[Scalar]) -> Scalar {
    let len = r.len();
    assert_eq!(len, self.size_point);
    (0..len)
      .map(|i| Scalar::from((len - i - 1).pow2() as u64) * r[i])
      .sum()
  }
}

impl DensePolynomial {
  pub fn new(Z: Vec<Scalar>) -> Self {
    let len = Z.len();
    let num_vars = len.log2();
    DensePolynomial { num_vars, Z, len }
  }

  pub fn get_num_vars(&self) -> usize {
    self.num_vars
  }

  pub fn len(&self) -> usize {
    self.len
  }

  pub fn clone(&self) -> DensePolynomial {
    DensePolynomial::new(self.Z[0..self.len].to_vec())
  }

  pub fn split(&self, idx: usize) -> (DensePolynomial, DensePolynomial) {
    assert!(idx < self.len());
    (
      DensePolynomial::new(self.Z[..idx].to_vec()),
      DensePolynomial::new(self.Z[idx..2 * idx].to_vec()),
    )
  }

  pub fn commit(
    &self,
    _gens: &PolyCommitmentGens,
    _random_tape: Option<&mut RandomTape>,
  ) -> (PolyCommitment, PolyDecommitment) {
    let n = self.Z.len();
    let ell = self.get_num_vars();
    assert_eq!(n, ell.pow2());

    //let enc = LigeroEncoding::new(coeffs.len());
    //let decomm = LigeroCommit::<Hasher, _>::commit(&coeffs, &enc).unwrap();
    let enc = LigeroEncoding::new_ml(self.num_vars);
    let decomm = LigeroCommit::<Hasher, _>::commit(&self.Z, &enc).unwrap();
    let C = decomm.get_root(); // this is the polynomial commitment
    (PolyCommitment { C }, PolyDecommitment { decomm, enc })
  }

  pub fn bound_poly_var_top(&mut self, r: &Scalar) {
    let n = self.len() / 2;
    for i in 0..n {
      self.Z[i] = self.Z[i] + *r * (self.Z[i + n] - self.Z[i]);
    }
    self.num_vars -= 1;
    self.len = n;
  }

  pub fn bound_poly_var_bot(&mut self, r: &Scalar) {
    let n = self.len() / 2;
    for i in 0..n {
      self.Z[i] = self.Z[2 * i] + *r * (self.Z[2 * i + 1] - self.Z[2 * i]);
    }
    self.num_vars -= 1;
    self.len = n;
  }

  // returns Z(r) in O(n) time
  pub fn evaluate(&self, r: &[Scalar]) -> Scalar {
    // r must have a value for each variable
    assert_eq!(r.len(), self.get_num_vars());
    let chis = EqPolynomial::new(r.to_vec()).evals();
    assert_eq!(chis.len(), self.Z.len());
    (0..chis.len()).map(|i| chis[i] * self.Z[i]).sum()
  }

  fn vec(&self) -> &Vec<Scalar> {
    &self.Z
  }

  pub fn extend(&mut self, other: &DensePolynomial) {
    // TODO: allow extension even when some vars are bound
    assert_eq!(self.Z.len(), self.len);
    let other_vec = other.vec();
    assert_eq!(other_vec.len(), self.len);
    self.Z.extend(other_vec);
    self.num_vars += 1;
    self.len *= 2;
    assert_eq!(self.Z.len(), self.len);
  }

  pub fn merge<'a, I>(polys: I) -> DensePolynomial
  where
    I: IntoIterator<Item = &'a DensePolynomial>,
  {
    let mut Z: Vec<Scalar> = Vec::new();
    for poly in polys.into_iter() {
      Z.extend(poly.vec());
    }

    // pad the polynomial with zero polynomial at the end
    Z.resize(Z.len().next_power_of_two(), Scalar::zero());

    DensePolynomial::new(Z)
  }

  pub fn from_usize(Z: &[usize]) -> Self {
    DensePolynomial::new(
      (0..Z.len())
        .map(|i| Scalar::from(Z[i] as u64))
        .collect::<Vec<Scalar>>(),
    )
  }
}

impl Index<usize> for DensePolynomial {
  type Output = Scalar;

  #[inline(always)]
  fn index(&self, _index: usize) -> &Scalar {
    &(self.Z[_index])
  }
}

impl AppendToTranscript for PolyCommitment {
  fn append_to_transcript(&self, label: &'static [u8], transcript: &mut Transcript) {
    transcript.append_message(label, b"poly_commitment_begin");
    transcript.append_message(b"poly_commitment_share", &self.C.as_ref());
    transcript.append_message(label, b"poly_commitment_end");
  }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PolyEvalProof {
  proof: LigeroEvalProof<Hasher, Scalar>,
  left_num_vars: usize,
  right_num_vars: usize,
}

impl PolyEvalProof {
  fn protocol_name() -> &'static [u8] {
    b"polynomial evaluation proof"
  }

  pub fn prove(
    poly: &DensePolynomial,
    decomm: &PolyDecommitment,
    blinds_opt: Option<&PolyCommitmentBlinds>,
    r: &[Scalar],                  // point at which the polynomial is evaluated
    _Zr: &Scalar,                  // evaluation of \widetilde{Z}(r)
    blind_Zr_opt: Option<&Scalar>, // specifies a blind for Zr
    _gens: &PolyCommitmentGens,
    transcript: &mut Transcript,
    _random_tape: &mut RandomTape,
  ) -> PolyEvalProof {
    transcript.append_protocol_name(PolyEvalProof::protocol_name());

    // assert vectors are of the right size
    assert_eq!(poly.get_num_vars(), r.len());

    // compute L and R
    let (left_num_vars, right_num_vars) = (
      decomm.decomm.get_n_rows().log2(),
      r.len() - decomm.decomm.get_n_rows().log2(),
    );
    let L_size = left_num_vars.pow2();
    let R_size = right_num_vars.pow2();

    let default_blinds = PolyCommitmentBlinds {
      blinds: vec![Scalar::zero(); L_size],
    };
    let blinds = blinds_opt.map_or(&default_blinds, |p| p);

    assert_eq!(blinds.blinds.len(), L_size);

    let zero = Scalar::zero();
    let _blind_Zr = blind_Zr_opt.map_or(&zero, |p| p);

    // compute the L and R vectors
    let L = EqPolynomial::new(r[..left_num_vars].to_vec()).evals();
    let R = EqPolynomial::new(r[left_num_vars..].to_vec()).evals();
    assert_eq!(L.len(), L_size);
    assert_eq!(R.len(), R_size);

    assert_eq!(decomm.decomm.get_n_rows(), L.len());

    // L is the outer tensor.  R is the inner tensor.
    let proof = decomm.decomm.prove(&L, &decomm.enc, transcript);

    if proof.is_err() {
      println!("{:?}", proof);
    }

    let proof = proof.unwrap();

    assert_eq!(decomm.decomm.get_n_per_row(), proof.get_n_per_row());
    assert_eq!(
      decomm.decomm.get_n_per_row() * decomm.decomm.get_n_rows(),
      1 << r.len()
    );

    assert_eq!(R.len(), decomm.decomm.get_n_per_row());

    PolyEvalProof {
      proof,
      left_num_vars,
      right_num_vars,
    }
  }

  pub fn verify(
    &self,
    _gens: &PolyCommitmentGens,
    transcript: &mut Transcript,
    r: &[Scalar],  // point at which the polynomial is evaluated
    eval: &Scalar, // commitment to \widetilde{Z}(r)
    comm: &PolyCommitment,
  ) -> Result<(), ProofVerifyError> {
    transcript.append_protocol_name(PolyEvalProof::protocol_name());

    // compute L and R
    let (left_num_vars, right_num_vars) = (self.left_num_vars, self.right_num_vars);
    assert_eq!(left_num_vars + right_num_vars, r.len());
    let L_size = left_num_vars.pow2();
    let R_size = right_num_vars.pow2();

    let L = EqPolynomial::new(r[..left_num_vars].to_vec()).evals();
    let R = EqPolynomial::new(r[left_num_vars..].to_vec()).evals();
    assert_eq!(L.len(), L_size);
    assert_eq!(R.len(), R_size);
    assert_eq!(R.len(), self.proof.get_n_per_row());
    let enc = LigeroEncoding::new_from_dims(self.proof.get_n_per_row(), self.proof.get_n_cols());
    let res = self
      .proof
      .verify(&comm.C.clone().into_raw(), &L, &R, &enc, transcript)
      .unwrap();

    if res == *eval {
      Ok(())
    } else {
      Err(ProofVerifyError::InternalError)
    }
  }

  pub fn verify_plain(
    &self,
    gens: &PolyCommitmentGens,
    transcript: &mut Transcript,
    r: &[Scalar], // point at which the polynomial is evaluated
    Zr: &Scalar,  // evaluation \widetilde{Z}(r)
    comm: &PolyCommitment,
  ) -> Result<(), ProofVerifyError> {
    self.verify(gens, transcript, r, &Zr, comm)
  }
}

#[cfg(test)]
mod tests {
  use super::super::scalar::ScalarFromPrimitives;
  use super::*;
  use rand_core::OsRng;

  fn evaluate_with_LR(Z: &Vec<Scalar>, r: &Vec<Scalar>) -> Scalar {
    let eq = EqPolynomial::new(r.to_vec());
    let (L, R) = eq.compute_factored_evals();

    let ell = r.len();
    // ensure ell is even
    assert!(ell % 2 == 0);
    // compute n = 2^\ell
    let n = ell.pow2();
    // compute m = sqrt(n) = 2^{\ell/2}
    let m = n.square_root();

    // compute vector-matrix product between L and Z viewed as a matrix
    let LZ = (0..m)
      .map(|i| (0..m).map(|j| L[j] * Z[j * m + i]).sum())
      .collect::<Vec<Scalar>>();

    // compute dot product between LZ and R
    (0..LZ.len()).map(|i| LZ[i] * R[i]).sum()
  }

  #[test]
  fn check_polynomial_evaluation() {
    let mut Z: Vec<Scalar> = Vec::new(); // Z = [1, 2, 1, 4]
    Z.push(Scalar::one());
    Z.push((2 as usize).to_scalar());
    Z.push((1 as usize).to_scalar());
    Z.push((4 as usize).to_scalar());
    // r = [4,3]
    let mut r: Vec<Scalar> = Vec::new();
    r.push((4 as usize).to_scalar());
    r.push((3 as usize).to_scalar());

    let eval_with_LR = evaluate_with_LR(&Z, &r);
    let poly = DensePolynomial::new(Z);

    let eval = poly.evaluate(&r);
    assert_eq!(eval, (28 as usize).to_scalar());
    assert_eq!(eval_with_LR, eval);
  }

  pub fn compute_factored_chis_at_r(r: &Vec<Scalar>) -> (Vec<Scalar>, Vec<Scalar>) {
    let mut L: Vec<Scalar> = Vec::new();
    let mut R: Vec<Scalar> = Vec::new();

    let ell = r.len();
    assert!(ell % 2 == 0); // ensure ell is even
    let n = ell.pow2();
    let m = n.square_root();

    // compute row vector L
    for i in 0..m {
      let mut chi_i = Scalar::one();
      for j in 0..ell / 2 {
        let bit_j = ((m * i) & (1 << (r.len() - j - 1))) > 0;
        if bit_j {
          chi_i *= r[j];
        } else {
          chi_i *= Scalar::one() - r[j];
        }
      }
      L.push(chi_i);
    }

    // compute column vector R
    for i in 0..m {
      let mut chi_i = Scalar::one();
      for j in ell / 2..ell {
        let bit_j = (i & (1 << (r.len() - j - 1))) > 0;
        if bit_j {
          chi_i *= r[j];
        } else {
          chi_i *= Scalar::one() - r[j];
        }
      }
      R.push(chi_i);
    }
    (L, R)
  }

  pub fn compute_chis_at_r(r: &Vec<Scalar>) -> Vec<Scalar> {
    let ell = r.len();
    let n = ell.pow2();
    let mut chis: Vec<Scalar> = Vec::new();
    for i in 0..n {
      let mut chi_i = Scalar::one();
      for j in 0..r.len() {
        let bit_j = (i & (1 << (r.len() - j - 1))) > 0;
        if bit_j {
          chi_i *= r[j];
        } else {
          chi_i *= Scalar::one() - r[j];
        }
      }
      chis.push(chi_i);
    }
    chis
  }

  pub fn compute_outerproduct(L: Vec<Scalar>, R: Vec<Scalar>) -> Vec<Scalar> {
    assert_eq!(L.len(), R.len());

    let mut O: Vec<Scalar> = Vec::new();
    let m = L.len();
    for i in 0..m {
      for j in 0..m {
        O.push(L[i] * R[j]);
      }
    }
    O
  }

  #[test]
  fn check_memoized_chis() {
    let mut csprng: OsRng = OsRng;

    let s = 10;
    let mut r: Vec<Scalar> = Vec::new();
    for _i in 0..s {
      r.push(Scalar::random(&mut csprng));
    }
    let chis = tests::compute_chis_at_r(&r);
    let chis_m = EqPolynomial::new(r).evals();
    assert_eq!(chis, chis_m);
  }

  #[test]
  fn check_factored_chis() {
    let mut csprng: OsRng = OsRng;

    let s = 10;
    let mut r: Vec<Scalar> = Vec::new();
    for _i in 0..s {
      r.push(Scalar::random(&mut csprng));
    }
    let chis = EqPolynomial::new(r.clone()).evals();
    let (L, R) = EqPolynomial::new(r).compute_factored_evals();
    let O = compute_outerproduct(L, R);
    assert_eq!(chis, O);
  }

  #[test]
  fn check_memoized_factored_chis() {
    let mut csprng: OsRng = OsRng;

    let s = 10;
    let mut r: Vec<Scalar> = Vec::new();
    for _i in 0..s {
      r.push(Scalar::random(&mut csprng));
    }
    let (L, R) = tests::compute_factored_chis_at_r(&r);
    let eq = EqPolynomial::new(r);
    let (L2, R2) = eq.compute_factored_evals();
    assert_eq!(L, L2);
    assert_eq!(R, R2);
  }

  /*#[test]
  fn check_polynomial_commit() {
    let mut Z: Vec<Scalar> = Vec::new(); // Z = [1, 2, 1, 4]
    Z.push((1 as usize).to_scalar());
    Z.push((2 as usize).to_scalar());
    Z.push((1 as usize).to_scalar());
    Z.push((4 as usize).to_scalar());

    let poly = DensePolynomial::new(Z);

    // r = [4,3]
    let mut r: Vec<Scalar> = Vec::new();
    r.push((4 as usize).to_scalar());
    r.push((3 as usize).to_scalar());
    let eval = poly.evaluate(&r);
    assert_eq!(eval, (28 as usize).to_scalar());

    let gens = PolyCommitmentGens::new(poly.get_num_vars(), b"test-two");
    let (poly_comm, poly_decomm) = poly.commit(&gens, None);

    let mut random_tape = RandomTape::new(b"proof");
    let mut prover_transcript = Transcript::new(b"example");
    let proof = PolyEvalProof::prove(
      &poly,
      &poly_decomm,
      None,
      &r,
      &eval,
      None,
      &gens,
      &mut prover_transcript,
      &mut random_tape,
    );

    let mut verifier_transcript = Transcript::new(b"example");
    assert!(proof
      .verify(&gens, &mut verifier_transcript, &r, &eval, &poly_comm)
      .is_ok());
  }*/

  #[test]
  fn check_polynomial_commit_large() {
    let mut Z: Vec<Scalar> = Vec::new();
    for _i in 0..4096 {
      Z.push((2 as usize).to_scalar());
    }
    let poly = DensePolynomial::new(Z);

    // r = [4,3]
    let mut r: Vec<Scalar> = Vec::new();
    for _i in 0..12 {
      r.push((4 as usize).to_scalar());
    }

    let eval = poly.evaluate(&r);

    let gens = PolyCommitmentGens::new(poly.get_num_vars(), b"test-two");
    let (poly_comm, poly_decomm) = poly.commit(&gens, None);

    let mut random_tape = RandomTape::new(b"proof");
    let mut prover_transcript = Transcript::new(b"example");
    let proof = PolyEvalProof::prove(
      &poly,
      &poly_decomm,
      None,
      &r,
      &eval,
      None,
      &gens,
      &mut prover_transcript,
      &mut random_tape,
    );

    let proof2 = PolyEvalProof::prove(
      &poly,
      &poly_decomm,
      None,
      &r,
      &eval,
      None,
      &gens,
      &mut prover_transcript,
      &mut random_tape,
    );

    let mut verifier_transcript = Transcript::new(b"example");
    assert!(proof
      .verify(&gens, &mut verifier_transcript, &r, &eval, &poly_comm)
      .is_ok());
    assert!(proof2
      .verify(&gens, &mut verifier_transcript, &r, &eval, &poly_comm)
      .is_ok());
  }
}
