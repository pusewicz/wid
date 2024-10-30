# frozen_string_literal: true

require_relative "spec_helper"

describe Wid::Tokenizer do
  def tokenize(string)
    tokenizer = Wid::Tokenizer.new(string)
    tokens = []
    while (token = tokenizer.next_token)
      tokens << token.to_a
    end

    tokens
  end

  it "tokenizes numbers" do
    %w[3 2.5].each do |number|
      _(tokenize(number)).must_equal [[:NUMBER, number]]
    end
  end

  it "tokenizes strings" do
    %w['string1' "string2"].each do |string|
      _(tokenize(string)).must_equal [[:STRING, string]]
    end
  end

  it "tokenizes booleans" do
    %w[true false].each do |bool|
      _(tokenize(bool)).must_equal [[:BOOL, bool]]
    end
  end

  it "tokenizes nil" do
    _(tokenize("nil")).must_equal [[:NIL, "nil"]]
  end

  it "tokenizes operators" do
    %w[+ -].each do |operator|
      _(tokenize(operator)).must_equal [[:OPERATOR, operator]]
    end
  end
end
